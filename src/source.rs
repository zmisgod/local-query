use anyhow::{bail, Context, Result};
use serde::de::{self, DeserializeSeed, SeqAccess, Visitor};
use serde_json::{Map, Value};
use std::{
    collections::HashSet,
    fmt,
    io::{BufRead, BufReader, Read},
    path::Path,
    str::FromStr,
};

#[derive(Clone, Copy)]
pub enum Format {
    Csv,
    Tsv,
    Json,
    Jsonl,
}
impl FromStr for Format {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "csv" => Ok(Self::Csv),
            "tsv" => Ok(Self::Tsv),
            "json" => Ok(Self::Json),
            "jsonl" | "ndjson" => Ok(Self::Jsonl),
            _ => bail!("Unsupported format {value}; use csv, tsv, json or jsonl"),
        }
    }
}
impl Format {
    pub fn infer(path: impl AsRef<Path>) -> Result<Self> {
        path.as_ref()
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .parse()
            .context("Cannot infer input format; specify --format")
    }
}

fn object(value: Value) -> Result<Map<String, Value>> {
    let value = if let Value::String(encoded) = value {
        serde_json::from_str(&encoded).context("Invalid JSON-encoded string")?
    } else {
        value
    };
    match value {
        Value::Object(row) => Ok(row),
        _ => bail!("Each record must be a JSON object or a string encoding an object"),
    }
}

pub fn read(
    reader: impl Read,
    format: Format,
    no_header: bool,
    callback: &mut dyn FnMut(Map<String, Value>) -> Result<bool>,
) -> Result<()> {
    match format {
        Format::Csv | Format::Tsv => {
            let mut csv = csv::ReaderBuilder::new()
                .has_headers(!no_header)
                .delimiter(if matches!(format, Format::Tsv) {
                    b'\t'
                } else {
                    b','
                })
                .from_reader(reader);
            let headers: Vec<String> = if no_header {
                (1..=csv.headers()?.len())
                    .map(|i| format!("column{i}"))
                    .collect()
            } else {
                csv.headers()?.iter().map(str::to_owned).collect()
            };
            let mut seen = HashSet::new();
            for header in &headers {
                if header.is_empty() || !seen.insert(header) {
                    bail!("CSV headers must be nonempty and unique: {header:?}");
                }
            }
            for (index, record) in csv.records().enumerate() {
                let record = record.with_context(|| format!("CSV record {}", index + 1))?;
                let row = headers
                    .iter()
                    .cloned()
                    .zip(record.iter().map(|s| Value::String(s.to_owned())))
                    .collect();
                if !callback(row).with_context(|| format!("CSV record {}", index + 1))? {
                    break;
                }
            }
        }
        Format::Jsonl => {
            for (index, line) in BufReader::new(reader).lines().enumerate() {
                let process = || -> Result<Option<Map<String, Value>>> {
                    let line = line?;
                    if line.trim().is_empty() {
                        return Ok(None);
                    }
                    Ok(Some(object(serde_json::from_str(&line)?)?))
                };
                if let Some(row) = process().with_context(|| format!("JSONL line {}", index + 1))? {
                    if !callback(row).with_context(|| format!("JSONL line {}", index + 1))? {
                        break;
                    }
                }
            }
        }
        Format::Json => {
            let mut deserializer = serde_json::Deserializer::from_reader(BufReader::new(reader));
            let mut stopped = false;
            let mut failure = None;
            let result = ArraySeed {
                callback,
                stopped: &mut stopped,
                failure: &mut failure,
            }
            .deserialize(&mut deserializer);
            if let Some(error) = failure {
                return Err(error);
            }
            if !stopped {
                result?;
                deserializer.end()?;
            }
        }
    }
    Ok(())
}

struct ArraySeed<'a> {
    callback: &'a mut dyn FnMut(Map<String, Value>) -> Result<bool>,
    stopped: &'a mut bool,
    failure: &'a mut Option<anyhow::Error>,
}
impl<'de> DeserializeSeed<'de> for ArraySeed<'_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<(), D::Error> {
        deserializer.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for ArraySeed<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON array of objects")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<(), A::Error> {
        let mut index = 0;
        while let Some(value) = seq.next_element::<Value>()? {
            index += 1;
            match object(value)
                .and_then(&mut *self.callback)
                .with_context(|| format!("JSON array element {index}"))
            {
                Ok(true) => {}
                // A private flag distinguishes intentional LIMIT termination from malformed input.
                Ok(false) => {
                    *self.stopped = true;
                    return Err(de::Error::custom("query complete"));
                }
                Err(error) => {
                    *self.failure = Some(error);
                    return Err(de::Error::custom("record processing failed"));
                }
            }
        }
        Ok(())
    }
}
