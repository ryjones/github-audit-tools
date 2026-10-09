//! Shared ground for the TAC election phases: the files they hand each other,
//! and who does not count as a person.
//!
//! Each phase writes its result as JSON so the next one can be re-run on its
//! own -- the API half of the work is the expensive part and should never have
//! to be repeated because a later phase was wrong. The files are written with
//! sorted keys and an indent, so a diff between two runs is readable.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Accounts that are automation, not people. A login ending in `[bot]` is
/// caught by pattern; these are the ones that look like users.
pub const BOT_LOGINS: [&str; 8] = [
    "actions-user",
    "dependabot",
    "dependabot-preview",
    "github-actions",
    "lfdt-bot",
    "renovate",
    "renovate-bot",
    "web-flow",
];

/// Addresses that are machinery. Commits authored by these are not anyone's
/// contribution.
pub const BOT_EMAILS: [&str; 4] = [
    "actions@github.com",
    "noreply@github.com",
    "support@github.com",
    "41898282+github-actions[bot]@users.noreply.github.com",
];

pub fn is_bot(login: &str) -> bool {
    let low = login.to_lowercase();
    low.ends_with("[bot]") || BOT_LOGINS.contains(&low.as_str())
}

pub fn is_bot_email(email: &str) -> bool {
    BOT_EMAILS.contains(&email)
}

/// The activity window, as the phases record it.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct Window {
    pub since: String,
    /// `now` when the run set no end, which is what the page means by "to
    /// today".
    pub until: String,
}

/// Today, locally. The cache records when a lookup was made, and local is what
/// a person reading it later will expect.
pub fn today() -> String {
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    let date = now.date();
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

/// Read JSON, or `None` when the file is absent or will not parse. A phase that
/// has not run yet and a phase that wrote nonsense are the same problem to the
/// phase after it: run it again.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write JSON with sorted keys through a temporary file in the same directory,
/// so an interrupt cannot truncate what was there before.
pub fn write_json<T: Serialize>(path: &Path, data: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    // Through Value first: serde_json's maps are sorted, so the output is
    // stable no matter what order the structs declare their fields in.
    let value: serde_json::Value = serde_json::to_value(data)?;
    let mut text = serde_json::to_string_pretty(&value)?;
    text.push('\n');

    let tmp = PathBuf::from(format!("{}.tmp.{}", path.display(), std::process::id()));
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} into place", tmp.display()))?;
    Ok(())
}

/// `{"a": 1}` in the order a human wants to read it: sorted, with an index wide
/// enough for the largest number.
pub fn sorted(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let set: BTreeSet<String> = values.into_iter().collect();
    set.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bot_is_a_bot_by_suffix_or_by_name() {
        for bot in [
            "dependabot[bot]",
            "Dependabot",
            "github-actions",
            "web-flow",
        ] {
            assert!(is_bot(bot), "{bot} should be a bot");
        }
        for person in ["ryjones", "robotics-fan", "botanist"] {
            assert!(!is_bot(person), "{person} should not be a bot");
        }
    }

    #[test]
    fn json_is_written_with_sorted_keys_and_a_trailing_newline() {
        let dir = std::env::temp_dir().join(format!("tac-json-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("out.json");

        #[derive(Serialize)]
        struct Unsorted {
            zebra: u8,
            apple: u8,
        }
        write_json(&path, &Unsorted { zebra: 1, apple: 2 }).expect("writes");
        let text = std::fs::read_to_string(&path).expect("reads back");
        assert_eq!(text, "{\n  \"apple\": 2,\n  \"zebra\": 1\n}\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_or_broken_file_reads_as_nothing() {
        let missing: Option<serde_json::Value> = read_json(Path::new("/nonexistent/x.json"));
        assert!(missing.is_none());
    }

    #[test]
    fn today_is_an_iso_date() {
        let date = today();
        assert_eq!(date.len(), 10, "{date}");
        assert!(date.chars().filter(|c| *c == '-').count() == 2, "{date}");
    }
}
