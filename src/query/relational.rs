use super::*;
use sqlparser::ast::{Distinct, Function, FunctionArg, FunctionArgExpr, GroupByExpr};
use std::collections::{HashMap, HashSet};

type Row = Map<String, Value>;

fn projection_expr(item: &SelectItem) -> Option<&Expr> {
    match item {
        SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => Some(e),
        _ => None,
    }
}

// Only expressions admitted by validate() reach these walkers.
fn children(expr: &Expr) -> Vec<&Expr> {
    match expr {
        Expr::Nested(e) | Expr::IsNull(e) | Expr::IsNotNull(e) | Expr::UnaryOp { expr: e, .. } => {
            vec![e]
        }
        Expr::BinaryOp { left, right, .. } => vec![left, right],
        Expr::Function(f) => f
            .args
            .iter()
            .filter_map(|arg| match arg {
                FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}
fn has_aggregate(expr: &Expr) -> bool {
    matches!(expr, Expr::Function(_)) || children(expr).into_iter().any(has_aggregate)
}
fn check_grouped(expr: &Expr, groups: &[Expr]) -> Result<()> {
    fn unnest(expr: &Expr) -> &Expr {
        match expr {
            Expr::Nested(e) => unnest(e),
            _ => expr,
        }
    }
    if groups.iter().any(|g| unnest(g) == unnest(expr)) || matches!(expr, Expr::Function(_)) {
        return Ok(());
    }
    ensure!(
        !matches!(expr, Expr::Identifier(_) | Expr::CompoundIdentifier(_)),
        "Field {expr} must appear in GROUP BY or an aggregate function"
    );
    for child in children(expr) {
        check_grouped(child, groups)?;
    }
    Ok(())
}

fn resolve_aliases(expr: &Expr, aliases: &HashMap<String, Expr>) -> Expr {
    match expr {
        Expr::Identifier(id) => aliases
            .get(&id.value)
            .cloned()
            .unwrap_or_else(|| expr.clone()),
        Expr::Nested(e) => Expr::Nested(Box::new(resolve_aliases(e, aliases))),
        Expr::IsNull(e) => Expr::IsNull(Box::new(resolve_aliases(e, aliases))),
        Expr::IsNotNull(e) => Expr::IsNotNull(Box::new(resolve_aliases(e, aliases))),
        Expr::UnaryOp { op, expr } => Expr::UnaryOp {
            op: *op,
            expr: Box::new(resolve_aliases(expr, aliases)),
        },
        Expr::BinaryOp { left, op, right } => Expr::BinaryOp {
            left: Box::new(resolve_aliases(left, aliases)),
            op: op.clone(),
            right: Box::new(resolve_aliases(right, aliases)),
        },
        // Aggregate arguments refer to input fields, not SELECT aliases.
        _ => expr.clone(),
    }
}
fn clause_expr(
    expr: &Expr,
    projection: &[SelectItem],
    aliases: &HashMap<String, Expr>,
) -> Result<Expr> {
    if let Expr::Value(SqlValue::Number(n, _)) = expr {
        let position = n
            .parse::<usize>()
            .context("Column position must be a positive integer")?;
        let item = position
            .checked_sub(1)
            .and_then(|p| projection.get(p))
            .context("Column position is outside SELECT list")?;
        return projection_expr(item)
            .cloned()
            .context("Column positions cannot refer to wildcard projections");
    }
    Ok(resolve_aliases(expr, aliases))
}

impl Plan {
    pub(super) fn prepare(&mut self) -> Result<()> {
        ensure!(
            !matches!(self.select.distinct, Some(Distinct::On(_))),
            "DISTINCT ON is not supported"
        );
        let mut aliases = HashMap::new();
        let mut names = HashSet::new();
        for item in &self.select.projection {
            let name = match item {
                SelectItem::ExprWithAlias { expr, alias } => {
                    aliases.insert(alias.value.clone(), expr.clone());
                    Some(alias.value.clone())
                }
                SelectItem::UnnamedExpr(Expr::Identifier(id)) => Some(id.value.clone()),
                SelectItem::UnnamedExpr(e) => Some(e.to_string()),
                _ => None,
            };
            if let Some(name) = name {
                ensure!(
                    names.insert(name.clone()),
                    "Duplicate output field {name:?}; use AS"
                );
            }
        }
        if let Some(e) = &self.select.selection {
            ensure!(
                !has_aggregate(e),
                "Aggregate functions are not allowed in WHERE; use HAVING"
            );
        }
        let GroupByExpr::Expressions(groups) = &self.select.group_by else {
            bail!("GROUP BY ALL is not supported")
        };
        self.groups = groups
            .iter()
            .map(|e| clause_expr(e, &self.select.projection, &aliases))
            .collect::<Result<_>>()?;
        for expr in &self.groups {
            validate(expr)?;
            ensure!(
                !has_aggregate(expr),
                "Aggregate functions are not allowed in GROUP BY"
            );
        }
        if let Some(expr) = &mut self.select.having {
            *expr = resolve_aliases(expr, &aliases);
            validate(expr)?;
        }
        for order in &mut self.order {
            order.expr = clause_expr(&order.expr, &self.select.projection, &aliases)?;
            validate(&order.expr)?;
        }
        self.grouped = !self.groups.is_empty()
            || self.select.having.is_some()
            || self
                .select
                .projection
                .iter()
                .filter_map(projection_expr)
                .any(has_aggregate)
            || self.order.iter().any(|o| has_aggregate(&o.expr));
        if self.grouped {
            for item in &self.select.projection {
                let expr = projection_expr(item)
                    .context("SELECT * is not allowed in grouped/aggregate queries")?;
                check_grouped(expr, &self.groups)?;
            }
            if let Some(expr) = &self.select.having {
                check_grouped(expr, &self.groups)?;
            }
            for order in &self.order {
                check_grouped(&order.expr, &self.groups)?;
            }
        }
        if self.select.distinct.is_some()
            && !self
                .select
                .projection
                .iter()
                .any(|i| matches!(i, SelectItem::Wildcard(_)))
        {
            for order in &self.order {
                ensure!(
                    self.select
                        .projection
                        .iter()
                        .filter_map(projection_expr)
                        .any(|e| e == &order.expr),
                    "With SELECT DISTINCT, ORDER BY expressions must appear in SELECT"
                );
            }
        }
        Ok(())
    }

    pub fn needs_buffer(&self) -> bool {
        self.grouped || !self.order.is_empty() || self.select.distinct.is_some()
    }

    // Input rows have already passed WHERE. Keep representative rows only for
    // expressions proven group-safe by prepare(), never arbitrary ungrouped fields.
    pub fn finish(&self, rows: Vec<Row>, csv: bool) -> Result<Vec<Value>> {
        let mut groups: Vec<Vec<Row>> = Vec::new();
        if self.grouped {
            if self.groups.is_empty() {
                groups.push(rows);
            } else {
                let mut indexes = HashMap::new();
                for row in rows {
                    let key = self
                        .groups
                        .iter()
                        .map(|e| eval(e, &row, csv).and_then(|v| key(&v, false)))
                        .collect::<Result<Vec<_>>>()?;
                    let index = *indexes.entry(key).or_insert_with(|| {
                        groups.push(Vec::new());
                        groups.len() - 1
                    });
                    groups[index].push(row);
                }
            }
        } else {
            groups = rows.into_iter().map(|r| vec![r]).collect();
        }

        let mut result = Vec::new();
        let mut seen = HashSet::new();
        for rows in groups {
            let empty = Map::new();
            let row = rows.first().unwrap_or(&empty);
            let group = self.grouped.then_some(rows.as_slice());
            if let Some(expr) = &self.select.having {
                if truth(&eval_in(expr, row, csv, group)?)? != Some(true) {
                    continue;
                }
            }
            let projected = self.project(row, csv, group)?;
            if self.select.distinct.is_some() && !seen.insert(key(&projected, false)?) {
                continue;
            }
            let keys = self
                .order
                .iter()
                .map(|o| eval_in(&o.expr, row, csv, group).and_then(|v| key(&v, csv)))
                .collect::<Result<Vec<_>>>()?;
            result.push((projected, keys));
        }
        // Precompute fallible keys before sorting; comparator is total and infallible.
        result.sort_by(|(_, a), (_, b)| {
            for ((a, b), order) in a.iter().zip(b).zip(&self.order) {
                let asc = order.asc.unwrap_or(true);
                let nulls_first = order.nulls_first.unwrap_or(!asc);
                let cmp = match (a == &Key::Null, b == &Key::Null) {
                    (true, true) => Ordering::Equal,
                    (true, false) => {
                        if nulls_first {
                            Ordering::Less
                        } else {
                            Ordering::Greater
                        }
                    }
                    (false, true) => {
                        if nulls_first {
                            Ordering::Greater
                        } else {
                            Ordering::Less
                        }
                    }
                    _ => {
                        if asc {
                            a.cmp(b)
                        } else {
                            b.cmp(a)
                        }
                    }
                };
                if cmp != Ordering::Equal {
                    return cmp;
                }
            }
            Ordering::Equal
        });
        Ok(result
            .into_iter()
            .take(self.limit.unwrap_or(usize::MAX))
            .map(|(v, _)| v)
            .collect())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Key {
    Null,
    Bool(bool),
    Number(BigDecimal),
    Text(String),
    Array(Vec<Key>),
    Object(Vec<(String, Key)>),
}
fn key(value: &Value, numeric_text: bool) -> Result<Key> {
    Ok(match value {
        Value::Null => Key::Null,
        Value::Bool(v) => Key::Bool(*v),
        Value::Number(v) => Key::Number(BigDecimal::from_str(&v.to_string())?),
        Value::String(v) => {
            if numeric_text {
                BigDecimal::from_str(v.trim())
                    .map(Key::Number)
                    .unwrap_or_else(|_| Key::Text(v.clone()))
            } else {
                Key::Text(v.clone())
            }
        }
        Value::Array(values) => Key::Array(
            values
                .iter()
                .map(|v| key(v, false))
                .collect::<Result<_>>()?,
        ),
        Value::Object(values) => Key::Object(
            values
                .iter()
                .map(|(k, v)| Ok((k.clone(), key(v, false)?)))
                .collect::<Result<_>>()?,
        ),
    })
}

pub(super) fn validate_function(f: &Function) -> Result<()> {
    let name = f.name.to_string().to_ascii_uppercase();
    ensure!(
        matches!(name.as_str(), "COUNT" | "SUM" | "AVG" | "MIN" | "MAX"),
        "Unsupported function: {}",
        f.name
    );
    ensure!(
        f.filter.is_none()
            && f.over.is_none()
            && f.null_treatment.is_none()
            && f.order_by.is_empty()
            && !f.special,
        "Aggregate FILTER, OVER and function-local ordering are not supported"
    );
    ensure!(f.args.len() == 1, "{name} requires exactly one argument");
    match &f.args[0] {
        FunctionArg::Unnamed(FunctionArgExpr::Wildcard) => ensure!(
            name == "COUNT" && !f.distinct,
            "Only COUNT(*) accepts a wildcard"
        ),
        FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => {
            validate(expr)?;
            ensure!(
                !has_aggregate(expr),
                "Nested aggregate functions are not supported"
            );
        }
        _ => bail!("Unsupported argument to {name}"),
    }
    Ok(())
}
fn number(value: &Value, csv: bool) -> Result<BigDecimal> {
    match value {
        Value::Number(n) => Ok(BigDecimal::from_str(&n.to_string())?),
        Value::String(s) if csv => {
            BigDecimal::from_str(s.trim()).context("Expected numeric CSV value")
        }
        _ => bail!("Expected a number, got {value}"),
    }
}
fn json_number(number: BigDecimal) -> Result<Value> {
    Ok(Value::Number(serde_json::Number::from_str(
        &number.normalized().to_string(),
    )?))
}
pub(super) fn arithmetic(a: &Value, b: &Value, op: &Op, csv: bool) -> Result<Value> {
    if a.is_null() || b.is_null() {
        return Ok(Value::Null);
    }
    let (a, b) = (number(a, csv)?, number(b, csv)?);
    if matches!(op, Op::Divide | Op::Modulo) {
        ensure!(b != 0, "Division by zero");
    }
    json_number(match op {
        Op::Plus => a + b,
        Op::Minus => a - b,
        Op::Multiply => a * b,
        Op::Divide => a / b,
        Op::Modulo => a % b,
        _ => unreachable!(),
    })
}
pub(super) fn aggregate(f: &Function, rows: &[Row], csv: bool) -> Result<Value> {
    let name = f.name.to_string().to_ascii_uppercase();
    let FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) = &f.args[0] else {
        return Ok(Value::from(rows.len()));
    };
    let mut values = Vec::new();
    let mut seen = HashSet::new();
    for row in rows {
        let value = eval(expr, row, csv)?;
        if value.is_null() {
            continue;
        }
        // SUM/AVG consume numeric CSV data; COUNT DISTINCT preserves raw text.
        let value = if matches!(name.as_str(), "SUM" | "AVG") {
            json_number(number(&value, csv)?)?
        } else {
            value
        };
        if f.distinct && !seen.insert(key(&value, false)?) {
            continue;
        }
        values.push(value);
    }
    if name == "COUNT" {
        return Ok(Value::from(values.len()));
    }
    if values.is_empty() {
        return Ok(Value::Null);
    }
    if name == "SUM" || name == "AVG" {
        let mut sum = BigDecimal::from(0);
        for value in &values {
            sum += number(value, csv)?;
        }
        if name == "AVG" {
            sum = sum / BigDecimal::from(values.len() as u64);
        }
        return json_number(sum);
    }
    let mut chosen = values.remove(0);
    let mut chosen_key = key(&chosen, csv)?;
    for value in values {
        let next = key(&value, csv)?;
        if (name == "MIN" && next < chosen_key) || (name == "MAX" && next > chosen_key) {
            chosen = value;
            chosen_key = next;
        }
    }
    Ok(chosen)
}
