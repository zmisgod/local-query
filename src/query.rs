mod relational;

use anyhow::{bail, ensure, Context, Result};
use bigdecimal::BigDecimal;
use serde_json::{Map, Value};
use sqlparser::{
    ast::{
        BinaryOperator as Op, Expr, OrderByExpr, Select, SelectItem, SetExpr, Statement,
        TableFactor, UnaryOperator, Value as SqlValue, WildcardAdditionalOptions,
    },
    dialect::{Dialect, GenericDialect},
    parser::Parser,
};
use std::{cmp::Ordering, str::FromStr};

// Keep generic identifier rules (including Chinese names), without database-specific
// special functions such as USER that would otherwise break the field user.city.
#[derive(Debug)]
struct FileDialect;
impl Dialect for FileDialect {
    fn is_identifier_start(&self, ch: char) -> bool {
        GenericDialect {}.is_identifier_start(ch)
    }
    fn is_identifier_part(&self, ch: char) -> bool {
        GenericDialect {}.is_identifier_part(ch)
    }
}

pub struct Plan {
    pub path: String,
    pub folder: bool,
    pub limit: Option<usize>,
    select: Select,
    order: Vec<OrderByExpr>,
    groups: Vec<Expr>,
    grouped: bool,
}
impl Plan {
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        let first = input.split_whitespace().next().unwrap_or("");
        let sql = if first.eq_ignore_ascii_case("select") {
            input.to_owned()
        } else if first.eq_ignore_ascii_case("lselect") {
            format!("SELECT {}", &input[first.len()..])
        } else {
            format!("SELECT {input}")
        };
        let (sql, path, folder) = extract_path(&sql)?;
        let mut ast = Parser::parse_sql(&FileDialect, &sql)?;
        ensure!(ast.len() == 1, "Exactly one SELECT query is required");
        let Statement::Query(query) = ast.remove(0) else {
            bail!("Only SELECT is supported")
        };
        ensure!(
            query.with.is_none()
                && query.limit_by.is_empty()
                && query.offset.is_none()
                && query.fetch.is_none()
                && query.locks.is_empty(),
            "WITH, OFFSET, FETCH and locking are not supported"
        );
        let limit = match query.limit {
            None => None,
            Some(Expr::Value(SqlValue::Number(n, _))) => Some(
                n.parse::<usize>()
                    .context("LIMIT must be a nonnegative integer")?,
            ),
            _ => bail!("LIMIT must be a nonnegative integer"),
        };
        let SetExpr::Select(select) = *query.body else {
            bail!("Only a single SELECT is supported")
        };
        ensure!(
            select.top.is_none()
                && select.into.is_none()
                && select.lateral_views.is_empty()
                && select.cluster_by.is_empty()
                && select.distribute_by.is_empty()
                && select.sort_by.is_empty()
                && select.named_window.is_empty()
                && select.qualify.is_none(),
            "TOP, INTO and window clauses are not supported"
        );
        ensure!(
            select.from.len() == 1 && select.from[0].joins.is_empty(),
            "Exactly one file is supported; JOIN is not supported"
        );
        match &select.from[0].relation {
            TableFactor::Table {
                name,
                alias,
                args,
                with_hints,
                version,
                partitions,
            } if name.to_string() == "__input_file"
                && alias.is_none()
                && args.is_none()
                && with_hints.is_empty()
                && version.is_none()
                && partitions.is_empty() => {}
            _ => bail!("FROM must contain only a file path; table aliases are not supported"),
        }
        for item in &select.projection {
            match item {
                SelectItem::Wildcard(opts) if opts == &WildcardAdditionalOptions::default() => {}
                SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                    validate(expr)?
                }
                _ => bail!("Unsupported projection: {item}"),
            }
        }
        if let Some(expr) = &select.selection {
            validate(expr)?;
        }
        let mut plan = Self {
            path,
            folder,
            limit,
            select: *select,
            order: query.order_by,
            groups: Vec::new(),
            grouped: false,
        };
        plan.prepare()?;
        Ok(plan)
    }

    pub fn matches(&self, row: &Map<String, Value>, csv: bool) -> Result<bool> {
        match &self.select.selection {
            Some(expr) => Ok(truth(&eval(expr, row, csv)?)? == Some(true)),
            None => Ok(true),
        }
    }

    pub fn apply(&self, row: &Map<String, Value>, csv: bool) -> Result<Option<Value>> {
        if !self.matches(row, csv)? {
            return Ok(None);
        }
        Ok(Some(self.project(row, csv, None)?))
    }

    fn project(
        &self,
        row: &Map<String, Value>,
        csv: bool,
        group: Option<&[Map<String, Value>]>,
    ) -> Result<Value> {
        let mut projected = Map::new();
        for item in &self.select.projection {
            match item {
                SelectItem::Wildcard(_) => {
                    for (key, value) in row {
                        insert(&mut projected, key.clone(), value.clone())?;
                    }
                }
                SelectItem::UnnamedExpr(expr) => {
                    let name = match expr {
                        Expr::Identifier(id) => id.value.clone(),
                        Expr::CompoundIdentifier(ids) => ids
                            .iter()
                            .map(|id| id.value.as_str())
                            .collect::<Vec<_>>()
                            .join("."),
                        _ => expr.to_string(),
                    };
                    insert(&mut projected, name, eval_in(expr, row, csv, group)?)?;
                }
                SelectItem::ExprWithAlias { expr, alias } => insert(
                    &mut projected,
                    alias.value.clone(),
                    eval_in(expr, row, csv, group)?,
                )?,
                _ => unreachable!(),
            }
        }
        Ok(Value::Object(projected))
    }
}
fn insert(row: &mut Map<String, Value>, name: String, value: Value) -> Result<()> {
    ensure!(
        !row.contains_key(&name),
        "Duplicate output field {name:?}; use AS to give it a unique name"
    );
    row.insert(name, value);
    Ok(())
}

// Scan SQL quotes before locating FROM, so literals/identifiers containing "from"
// cannot be mistaken for the source. Replace only the path with a SQL table token.
fn extract_path(sql: &str) -> Result<(String, String, bool)> {
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if matches!(bytes[i], b'\'' | b'"' | b'`') {
            let (_, end) = quoted(sql, i)?;
            i = end;
            continue;
        }
        if FileDialect.is_identifier_start(sql[i..].chars().next().unwrap()) {
            let start = i;
            while i < bytes.len() {
                let ch = sql[i..].chars().next().unwrap();
                if !FileDialect.is_identifier_part(ch) {
                    break;
                }
                i += ch.len_utf8();
            }
            if sql[start..i].eq_ignore_ascii_case("from") {
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                ensure!(i < bytes.len(), "Missing file path after FROM");
                let start = i;
                let folder = sql[i..]
                    .get(..6)
                    .is_some_and(|s| s.eq_ignore_ascii_case("folder"))
                    && bytes.get(i + 6).is_none_or(|b| b.is_ascii_whitespace());
                if folder {
                    i += 6;
                    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    ensure!(
                        i < bytes.len() && matches!(bytes[i], b'\'' | b'"' | b'`'),
                        "FROM FOLDER requires a quoted directory path"
                    );
                }
                let path_start = i;
                let (path, end) = if matches!(bytes[i], b'\'' | b'"' | b'`') {
                    quoted(sql, i)?
                } else {
                    while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b';' {
                        i += 1;
                    }
                    (sql[path_start..i].to_owned(), i)
                };
                ensure!(!path.is_empty(), "File path must not be empty");
                return Ok((
                    format!("{} __input_file {}", &sql[..start], &sql[end..]),
                    path,
                    folder,
                ));
            }
        } else {
            // Comments are deliberately rejected to keep source extraction unambiguous.
            ensure!(
                !sql[i..].starts_with("--") && !sql[i..].starts_with("/*"),
                "SQL comments are not supported"
            );
            i += sql[i..].chars().next().unwrap().len_utf8();
        }
    }
    bail!("Missing FROM clause")
}
fn quoted(sql: &str, start: usize) -> Result<(String, usize)> {
    let quote = sql.as_bytes()[start];
    let mut i = start + 1;
    let mut value = String::new();
    while i < sql.len() {
        if sql.as_bytes()[i] == quote {
            if sql.as_bytes().get(i + 1) == Some(&quote) {
                value.push(quote as char);
                i += 2;
            } else {
                return Ok((value, i + 1));
            }
        } else {
            let ch = sql[i..].chars().next().unwrap();
            value.push(ch);
            i += ch.len_utf8();
        }
    }
    bail!("Unterminated quoted value")
}

fn validate(expr: &Expr) -> Result<()> {
    match expr {
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) => Ok(()),
        Expr::Value(
            SqlValue::Number(_, _)
            | SqlValue::SingleQuotedString(_)
            | SqlValue::Boolean(_)
            | SqlValue::Null,
        ) => Ok(()),
        Expr::Nested(e) | Expr::IsNull(e) | Expr::IsNotNull(e) => validate(e),
        Expr::UnaryOp {
            op: UnaryOperator::Not | UnaryOperator::Minus | UnaryOperator::Plus,
            expr,
        } => validate(expr),
        Expr::BinaryOp {
            left,
            op:
                Op::Eq
                | Op::NotEq
                | Op::Lt
                | Op::LtEq
                | Op::Gt
                | Op::GtEq
                | Op::And
                | Op::Or
                | Op::Plus
                | Op::Minus
                | Op::Multiply
                | Op::Divide
                | Op::Modulo,
            right,
        } => {
            validate(left)?;
            validate(right)
        }
        Expr::Function(function) => relational::validate_function(function),
        _ => bail!("Unsupported expression: {expr}"),
    }
}
fn truth(value: &Value) -> Result<Option<bool>> {
    match value {
        Value::Null => Ok(None),
        Value::Bool(v) => Ok(Some(*v)),
        _ => bail!("WHERE, AND, OR and NOT require boolean expressions"),
    }
}
fn boolean(value: Option<bool>) -> Value {
    value.map(Value::Bool).unwrap_or(Value::Null)
}
fn eval(expr: &Expr, row: &Map<String, Value>, csv: bool) -> Result<Value> {
    eval_in(expr, row, csv, None)
}
fn eval_in(
    expr: &Expr,
    row: &Map<String, Value>,
    csv: bool,
    group: Option<&[Map<String, Value>]>,
) -> Result<Value> {
    Ok(match expr {
        Expr::Identifier(id) => row.get(&id.value).cloned().unwrap_or(Value::Null),
        Expr::CompoundIdentifier(ids) => {
            let mut value = row.get(&ids[0].value);
            for id in &ids[1..] {
                value = value
                    .and_then(|v| v.as_object())
                    .and_then(|v| v.get(&id.value));
            }
            value.cloned().unwrap_or(Value::Null)
        }
        Expr::Value(value) => match value {
            SqlValue::Number(n, _) => serde_json::from_str(n).context("Invalid numeric literal")?,
            SqlValue::SingleQuotedString(s) => Value::String(s.clone()),
            SqlValue::Boolean(b) => Value::Bool(*b),
            SqlValue::Null => Value::Null,
            _ => unreachable!(),
        },
        Expr::Nested(e) => eval_in(e, row, csv, group)?,
        Expr::IsNull(e) => Value::Bool(eval_in(e, row, csv, group)?.is_null()),
        Expr::IsNotNull(e) => Value::Bool(!eval_in(e, row, csv, group)?.is_null()),
        Expr::UnaryOp { op, expr } => {
            let v = eval_in(expr, row, csv, group)?;
            match op {
                UnaryOperator::Not => boolean(truth(&v)?.map(|v| !v)),
                _ if v.is_null() => Value::Null,
                UnaryOperator::Plus => relational::arithmetic(&Value::from(0), &v, &Op::Plus, csv)?,
                UnaryOperator::Minus => {
                    relational::arithmetic(&Value::from(0), &v, &Op::Minus, csv)?
                }
                _ => bail!("Unary {op} requires a number"),
            }
        }
        Expr::BinaryOp { left, op, right } => {
            let left = eval_in(left, row, csv, group)?;
            let right = eval_in(right, row, csv, group)?;
            match op {
                Op::And | Op::Or => {
                    let (a, b) = (truth(&left)?, truth(&right)?);
                    boolean(match op {
                        Op::And => {
                            if a == Some(false) || b == Some(false) {
                                Some(false)
                            } else if a == Some(true) && b == Some(true) {
                                Some(true)
                            } else {
                                None
                            }
                        }
                        _ => {
                            if a == Some(true) || b == Some(true) {
                                Some(true)
                            } else if a == Some(false) && b == Some(false) {
                                Some(false)
                            } else {
                                None
                            }
                        }
                    })
                }
                Op::Plus | Op::Minus | Op::Multiply | Op::Divide | Op::Modulo => {
                    relational::arithmetic(&left, &right, op, csv)?
                }
                _ => compare(&left, &right, op, csv)?,
            }
        }
        Expr::Function(function) => {
            relational::aggregate(function, group.context("Aggregate requires a group")?, csv)?
        }
        _ => unreachable!(),
    })
}
fn compare(a: &Value, b: &Value, op: &Op, csv: bool) -> Result<Value> {
    if a.is_null() || b.is_null() {
        return Ok(Value::Null);
    }
    let ordering = match (a, b) {
        (Value::Number(a), Value::Number(b)) => {
            Some(BigDecimal::from_str(&a.to_string())?.cmp(&BigDecimal::from_str(&b.to_string())?))
        }
        (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
        (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
        (Value::String(a), Value::Number(b)) if csv => {
            let b = BigDecimal::from_str(&b.to_string())?;
            BigDecimal::from_str(a.trim()).ok().map(|a| a.cmp(&b))
        }
        (Value::Number(a), Value::String(b)) if csv => {
            let a = BigDecimal::from_str(&a.to_string())?;
            BigDecimal::from_str(b.trim()).ok().map(|b| a.cmp(&b))
        }
        // Incompatible types (including nonnumeric CSV cells) yield SQL UNKNOWN.
        _ => None,
    };
    Ok(boolean(ordering.map(|order| match op {
        Op::Eq => order == Ordering::Equal,
        Op::NotEq => order != Ordering::Equal,
        Op::Lt => order == Ordering::Less,
        Op::LtEq => order != Ordering::Greater,
        Op::Gt => order == Ordering::Greater,
        Op::GtEq => order != Ordering::Less,
        _ => unreachable!(),
    })))
}
