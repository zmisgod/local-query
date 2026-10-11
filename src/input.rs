use crate::{
    query::Plan,
    source::{self, Format},
};
use anyhow::{ensure, Context, Result};
use serde_json::{Map, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub struct Inputs {
    files: Vec<(PathBuf, Format)>,
    pub csv: bool,
}
impl Inputs {
    pub fn resolve(
        plan: &Plan,
        format: Option<Format>,
        recursive: bool,
        no_header: bool,
    ) -> Result<Self> {
        ensure!(
            !recursive || plan.folder,
            "--recursive requires FROM FOLDER"
        );
        let path = Path::new(&plan.path);
        let mut paths = Vec::new();
        if plan.folder {
            ensure!(path.is_dir(), "Not a directory: {}", path.display());
            collect(path, recursive, &mut paths)?;
            paths.sort();
        } else {
            ensure!(
                path.is_file(),
                "Not a regular file: {}; use FROM FOLDER for directories",
                path.display()
            );
            // Preserve the single-file open check even for LIMIT 0.
            fs::File::open(path).with_context(|| format!("Cannot open {}", path.display()))?;
            paths.push(path.to_path_buf());
        }
        let mut files = Vec::new();
        for path in paths {
            let file_format = match format {
                Some(format) => format,
                None => match Format::infer(&path) {
                    Ok(format) => format,
                    Err(_) if plan.folder => continue,
                    Err(error) => {
                        return Err(error).context("Cannot infer input format; specify --format")
                    }
                },
            };
            ensure!(
                !no_header || is_csv(file_format),
                "--no-header only applies to CSV/TSV: {}",
                path.display()
            );
            files.push((path, file_format));
        }
        if let Some(format) = format {
            ensure!(
                !no_header || is_csv(format),
                "--no-header only applies to CSV/TSV"
            );
        }
        let csv = files.first().map(|(_, f)| is_csv(*f)).unwrap_or(false);
        ensure!(files.iter().all(|(_, f)| is_csv(*f) == csv),
            "Folder mixes CSV/TSV and JSON/JSONL; query them in separate folders to preserve type semantics");
        Ok(Self { files, csv })
    }

    pub fn read(
        &self,
        no_header: bool,
        callback: &mut dyn FnMut(Map<String, Value>) -> Result<bool>,
    ) -> Result<()> {
        for (path, format) in &self.files {
            let file =
                fs::File::open(path).with_context(|| format!("Cannot open {}", path.display()))?;
            let mut keep_reading = true;
            source::read(file, *format, no_header, &mut |row| {
                keep_reading = callback(row)?;
                Ok(keep_reading)
            })
            .with_context(|| format!("Reading {}", path.display()))?;
            if !keep_reading {
                break;
            }
        }
        Ok(())
    }
}
fn is_csv(format: Format) -> bool {
    matches!(format, Format::Csv | Format::Tsv)
}
fn collect(folder: &Path, recursive: bool, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in
        fs::read_dir(folder).with_context(|| format!("Cannot list {}", folder.display()))?
    {
        let entry = entry.with_context(|| format!("Cannot list {}", folder.display()))?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let kind = entry
            .file_type()
            .with_context(|| format!("Cannot inspect {}", entry.path().display()))?;
        // Do not follow symlinks: avoids cycles and duplicate traversal.
        if kind.is_file() {
            files.push(entry.path());
        } else if kind.is_dir() && recursive {
            collect(&entry.path(), true, files)?;
        }
    }
    Ok(())
}
