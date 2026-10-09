//! Where generated output goes.
//!
//! Every run writes its findings to a results directory (`results/` by
//! default) as timestamped files, so scans can be compared over time and so a
//! long scan's output survives a lost terminal. Console output is unchanged —
//! these files are written in addition to it.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;

pub struct Results {
    dir: PathBuf,
    /// `<command>-<scope>-<timestamp>`, the shared stem for this run's files.
    stem: String,
}

impl Results {
    /// Create the results directory and reserve a filename stem for this run.
    pub fn new(dir: &str, command: &str, scope: &str) -> Result<Self> {
        let dir = PathBuf::from(dir);
        fs::create_dir_all(&dir)
            .with_context(|| format!("creating results directory {}", dir.display()))?;
        Ok(Self {
            dir,
            stem: format!("{command}-{}-{}", slug(scope), timestamp()),
        })
    }

    /// Write one file for this run and report where it landed.
    pub fn write(&self, extension: &str, contents: &str) -> Result<PathBuf> {
        let path = self.dir.join(format!("{}.{extension}", self.stem));
        fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }
}

/// Announce a written file on stderr, so stdout stays pipeable.
pub fn announce(path: &Path) {
    eprintln!("wrote {}", path.display());
}

/// UTC `YYYYMMDD-HHMMSS`, for sortable filenames.
fn timestamp() -> String {
    let now = OffsetDateTime::now_utc();
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        now.year(),
        now.month() as u8,
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

/// Make an arbitrary scope (`my-org`, `OWNER/NAME`) filename-safe.
fn slug(scope: &str) -> String {
    let cleaned: String = scope
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "scan".to_string()
    } else {
        trimmed
    }
}
