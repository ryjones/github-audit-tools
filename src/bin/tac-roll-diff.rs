//! The commit message for a publish of the election data: who joined each
//! roll, who left, and why.
//!
//! `tac-eligibility` writes `voters.json` and `logins.json` into its `--repos`
//! directory. Publishing them somewhere that keeps history -- the
//! `tac-election-data` repository, whose copy is the public record -- is worth
//! doing only if each commit says what moved, so this compares the pair about
//! to be published against the pair already there:
//!
//!   tac-roll-diff --before tac-election-data/repos --after repos \
//!                 --source github-audit-tools@$(git rev-parse --short HEAD)
//!
//! and prints the message on stdout. `--before` may be a directory that holds
//! neither file, which is the first publish: the message says so rather than
//! reporting five hundred people as having just joined.
//!
//! No network, no git, no writes: it reads two files and prints.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;

use github_audit_tools::tac_logins::Logins;
use github_audit_tools::tac_rolldiff::{self as rolldiff, Snapshot};
use github_audit_tools::tac_voters::Voters;

/// Print the commit message describing how the election rolls moved.
#[derive(Debug, Parser)]
#[command(name = "tac-roll-diff", version, long_about = None)]
struct Args {
    /// The published copy: a directory holding voters.json and logins.json.
    /// Missing files mean a first publish.
    #[arg(long, value_name = "DIR")]
    before: Option<PathBuf>,

    /// The new copy, as `tac-eligibility --repos` left it.
    #[arg(long, value_name = "DIR", default_value = "repos")]
    after: PathBuf,

    /// Where the data came from, for the message -- conventionally
    /// `github-audit-tools@<short sha>`.
    #[arg(long, value_name = "TEXT")]
    source: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let (voters, nominees) = read(&args.after)?;
    let (voters, nominees) = (
        voters.with_context(|| format!("no voters.json in {}", args.after.display()))?,
        nominees.with_context(|| format!("no logins.json in {}", args.after.display()))?,
    );
    let after = Snapshot {
        voters: rolldiff::Roll::of_voters(&voters),
        nominees: rolldiff::Roll::of_nominees(&nominees),
    };

    // A published copy with one file and not the other is a half-written
    // publish; compare only what is there, and treat the rest as new.
    let before = match &args.before {
        None => None,
        Some(dir) => match read(dir)? {
            (None, None) => None,
            (old_voters, old_nominees) => Some(Snapshot {
                voters: old_voters
                    .map(|voters| rolldiff::Roll::of_voters(&voters))
                    .unwrap_or_default(),
                nominees: old_nominees
                    .map(|nominees| rolldiff::Roll::of_nominees(&nominees))
                    .unwrap_or_default(),
            }),
        },
    };

    print!(
        "{}",
        rolldiff::message(
            before.as_ref(),
            &after,
            &nominees.window,
            args.source.as_deref()
        )
    );
    Ok(())
}

/// The two files a roll comparison needs, each `None` when it is not there.
///
/// A file that exists and will not parse is an error: a publish that silently
/// treats yesterday's roll as absent would report everyone as having joined.
fn read(dir: &Path) -> Result<(Option<Voters>, Option<Logins>)> {
    Ok((
        parse(&dir.join("voters.json"))?,
        parse(&dir.join("logins.json"))?,
    ))
}

fn parse<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let parsed = serde_json::from_str(&text)
        .with_context(|| format!("{} is not what this tool writes", path.display()))?;
    Ok(Some(parsed))
}
