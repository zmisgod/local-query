mod input;
mod query;
mod source;

use anyhow::{bail, Context, Result};
use query::Plan;
use source::Format;
use std::io::{self, Write};

const HELP: &str = r#"Usage: lselect [--format csv|tsv|json|jsonl] [--no-header] [--recursive] '<columns> FROM [FOLDER] <path> [WHERE condition] [GROUP BY columns] [HAVING condition] [ORDER BY columns] [LIMIT n]'

Examples:
  lselect "* from '../../data.csv' where name = '张三'"
  lselect --no-header "column1, column3 from data.csv where column2 = 2"
  lselect "name, user.city from users.json where age >= 18 limit 10"
  lselect --format jsonl "* from events.txt where active = true"
  lselect --format jsonl "DISTINCT level from folder './logs'"
  lselect "city, COUNT(*) AS n from users.json group by city having n > 1 order by n desc"

An optional SELECT prefix is accepted. Quote the whole query to protect it from
shell expansion. Output is a streaming JSON array; errors go to stderr.
CSV/TSV assume a header unless --no-header is supplied. JSON rows must be objects
(or strings encoding objects). Missing fields are NULL. See README.md for details."#;

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut format = None;
    let mut no_header = false;
    let mut recursive = false;
    let mut sql = None;
    let mut options = true;
    while let Some(arg) = args.next() {
        if options && arg == "--" {
            options = false;
            continue;
        }
        if !options {
            if sql.replace(arg).is_some() {
                bail!("Pass the entire query as one quoted argument");
            }
            continue;
        }
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{HELP}");
                return Ok(());
            }
            "--format" => {
                format = Some(
                    args.next()
                        .context("--format requires a value")?
                        .parse::<Format>()?,
                )
            }
            "--no-header" => no_header = true,
            "--recursive" => recursive = true,
            _ if arg.starts_with('-') => bail!("Unknown option: {arg}"),
            _ => {
                if sql.replace(arg).is_some() {
                    bail!("Pass the entire query as one quoted argument");
                }
            }
        }
    }
    let sql = sql.context("Missing query. Run lselect --help for examples")?;
    let plan = Plan::parse(&sql)?;
    let inputs = input::Inputs::resolve(&plan, format, recursive, no_header)?;
    let stdout = io::stdout();
    let mut output = io::BufWriter::new(stdout.lock());
    execute(&plan, &inputs, no_header, &mut output)?;
    output.flush()?;
    Ok(())
}

fn execute(
    plan: &Plan,
    inputs: &input::Inputs,
    no_header: bool,
    output: &mut impl Write,
) -> Result<()> {
    output.write_all(b"[")?;
    let mut count = 0usize;
    let csv = inputs.csv;
    let mut emit = |row: &serde_json::Value| -> Result<()> {
        if count > 0 {
            output.write_all(b",")?;
        }
        output.write_all(b"\n")?;
        serde_json::to_writer(&mut *output, row)?;
        count += 1;
        Ok(())
    };
    if plan.limit != Some(0) {
        if plan.needs_buffer() {
            let mut rows = Vec::new();
            inputs
                .read(no_header, &mut |row| {
                    if plan.matches(&row, csv)? {
                        rows.push(row);
                    }
                    Ok(true)
                })
                .with_context(|| format!("Reading {}", plan.path))?;
            for row in plan.finish(rows, csv)? {
                emit(&row)?;
            }
        } else {
            let mut matched = 0usize;
            inputs
                .read(no_header, &mut |row| {
                    if let Some(row) = plan.apply(&row, csv)? {
                        emit(&row)?;
                        matched += 1;
                    }
                    Ok(plan.limit.is_none_or(|limit| matched < limit))
                })
                .with_context(|| format!("Reading {}", plan.path))?;
        }
    }
    output.write_all(b"\n]\n")?;
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}
