//! Who may stand: anyone who authored a commit in the window.
//!
//! The commits come out of the bare mirrors `clone-repos.zsh` made, with
//! `git log --all --since`, which needs no trees -- so a `--filter=tree:0`
//! mirror answers the question without ever fetching file content, and without
//! the network.
//!
//! A commit names an address, not an account, so this phase collects addresses
//! and one sample commit for each. Turning those into logins is the next
//! phase's job, and the sample is what it asks GitHub about.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::tac::{Window, is_bot_email};

/// Between the fields of one commit, and between co-authors within one commit.
/// Both are characters git will not produce itself.
const UNIT: char = '\x1f';
const RECORD: char = '\x1e';

/// One address, and what it did in the window.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Author {
    pub commits: usize,
    /// The name on the commit, for recognizing a person in the unresolved list.
    pub name: String,
    /// `org/repo` for each mirror the address appears in.
    pub repos: Vec<String>,
    /// `org/repo@sha` of one commit by this address, which is what the API is
    /// asked about. Absent for a co-author, who has no commit of their own.
    pub sample: Option<String>,
}

/// `authors.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Authors {
    pub window: Window,
    /// The reports that scoped the run, if any.
    pub reports: Vec<String>,
    pub mirrors: usize,
    pub coauthors: bool,
    pub commits: usize,
    /// Keyed by lowercased address.
    pub emails: BTreeMap<String, Author>,
}

/// How `clone-repos.zsh` spells a repository on disk: a leading dot becomes an
/// underscore, so `.github` is kept as `_github`.
pub fn on_disk(name: &str) -> String {
    match name.strip_prefix('.') {
        Some(rest) => format!("_{rest}"),
        None => name.to_string(),
    }
}

/// A repository named by a `gh-enterprise-repos` report.
#[derive(Debug, Deserialize)]
struct ReportRepo {
    org: String,
    name: String,
    #[serde(default)]
    visibility: Option<String>,
    #[serde(default)]
    archived: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ReportFile {
    repositories: Vec<ReportRepo>,
}

/// `(org, on-disk name)` for every public, non-archived repository the reports
/// name, or `None` when no report was given and every mirror counts.
///
/// A report is YAML or JSON; YAML parses both, so one path covers it.
pub fn wanted_from_reports(paths: &[PathBuf]) -> Result<Option<Vec<(String, String)>>> {
    if paths.is_empty() {
        return Ok(None);
    }
    let mut wanted = std::collections::BTreeSet::new();
    for path in paths {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let report: ReportFile = serde_yaml_ng::from_str(&text).map_err(|problem| {
            anyhow::anyhow!(
                "{} is not a gh-enterprise-repos report: {problem}",
                path.display()
            )
        })?;
        for repo in report.repositories {
            let public = repo.visibility.as_deref() == Some("PUBLIC");
            if public && repo.archived != Some(true) {
                wanted.insert((repo.org, on_disk(&repo.name)));
            }
        }
    }
    Ok(Some(wanted.into_iter().collect()))
}

/// One mirror on disk.
#[derive(Debug, Clone)]
pub struct Repo {
    pub org: String,
    pub name: String,
    pub path: PathBuf,
}

impl Repo {
    pub fn slug(&self) -> String {
        format!("{}/{}", self.org, self.name)
    }
}

/// Every bare clone under `repos/<org>/<name>`, kept to the ones the reports
/// name when there are any. A `.partial` directory is a clone that was
/// interrupted, and a directory with no `objects` is not a git repository.
pub fn mirrors(root: &Path, wanted: Option<&[(String, String)]>) -> Result<Vec<Repo>> {
    let mut found = Vec::new();
    for org_dir in sorted_dirs(root)? {
        let org = file_name(&org_dir);
        for repo_dir in sorted_dirs(&org_dir)? {
            let name = file_name(&repo_dir);
            if name.ends_with(".partial") || !repo_dir.join("objects").is_dir() {
                continue;
            }
            if let Some(wanted) = wanted
                && !wanted.iter().any(|(o, n)| *o == org && *n == name)
            {
                continue;
            }
            found.push(Repo {
                org: org.clone(),
                name,
                path: repo_dir,
            });
        }
    }
    Ok(found)
}

fn sorted_dirs(path: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(path)
        .with_context(|| format!("reading {}", path.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    Ok(dirs)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// One commit as `git log` reports it.
#[derive(Debug)]
pub struct Commit {
    pub sha: String,
    pub email: String,
    pub name: String,
    /// `Name <address>` for each `Co-authored-by` trailer, when they were asked
    /// for.
    pub coauthors: Vec<String>,
}

/// Every commit in the window, from one mirror.
///
/// `GIT_NO_LAZY_FETCH` keeps a partial clone from reaching for the trees it was
/// deliberately cloned without, and `GIT_TERMINAL_PROMPT` keeps a repository
/// that has gone private from stopping to ask for a password.
pub fn commits_of(
    path: &Path,
    since: &str,
    until: Option<&str>,
    coauthors: bool,
) -> Result<Vec<Commit>> {
    let mut fields = vec!["%H".to_string(), "%aE".to_string(), "%aN".to_string()];
    if coauthors {
        fields.push(format!(
            "%(trailers:key=Co-authored-by,valueonly,separator={RECORD})"
        ));
    }
    let mut command = std::process::Command::new("git");
    command
        .args(["-C"])
        .arg(path)
        .args(["log", "--all", "--no-notes"])
        .arg(format!("--since={since}"))
        .arg(format!("--format={}", fields.join(&UNIT.to_string())))
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
    if let Some(until) = until {
        command.arg(format!("--until={until}"));
    }

    let done = command
        .output()
        .with_context(|| format!("running git log in {}", path.display()))?;
    if !done.status.success() {
        let stderr = String::from_utf8_lossy(&done.stderr);
        let last = stderr.trim().lines().last().unwrap_or("git log failed");
        bail!("{last}");
    }

    let stdout = String::from_utf8_lossy(&done.stdout);
    Ok(stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut parts = line.split(UNIT);
            Commit {
                sha: parts.next().unwrap_or_default().to_string(),
                email: parts.next().unwrap_or_default().trim().to_lowercase(),
                name: parts.next().unwrap_or_default().trim().to_string(),
                coauthors: parts
                    .next()
                    .filter(|trailers| !trailers.is_empty())
                    .map(|trailers| trailers.split(RECORD).map(str::to_string).collect())
                    .unwrap_or_default(),
            }
        })
        .collect())
}

/// `Name <address>` split into the address, lowercased, and the name.
pub fn parse_trailer(trailer: &str) -> Option<(String, String)> {
    let trailer = trailer.trim();
    let open = trailer.rfind('<')?;
    let close = trailer.rfind('>')?;
    if close < open {
        return None;
    }
    let email = trailer[open + 1..close].trim().to_lowercase();
    if email.is_empty() {
        return None;
    }
    Some((email, trailer[..open].trim().to_string()))
}

/// What one mirror contributed, before it is merged with the rest.
pub fn addresses_in(clone: &Repo, commits: &[Commit], coauthors: bool) -> BTreeMap<String, Author> {
    let slug = clone.slug();
    let mut seen: BTreeMap<String, Author> = BTreeMap::new();
    for commit in commits {
        // The author, then anyone they credited. A co-author has no commit of
        // their own to look up, so they bring no sample sha.
        let mut people: Vec<(String, String, Option<&str>)> = vec![(
            commit.email.clone(),
            commit.name.clone(),
            Some(commit.sha.as_str()),
        )];
        if coauthors {
            for trailer in &commit.coauthors {
                if let Some((email, name)) = parse_trailer(trailer) {
                    people.push((email, name, None));
                }
            }
        }
        for (email, name, own) in people {
            if email.is_empty() || is_bot_email(&email) {
                continue;
            }
            let entry = seen.entry(email).or_insert_with(|| Author {
                commits: 0,
                name,
                repos: Vec::new(),
                sample: None,
            });
            entry.commits += 1;
            if let Some(sha) = own
                && entry.sample.is_none()
            {
                entry.sample = Some(format!("{slug}@{sha}"));
            }
        }
    }
    seen
}

/// What a sweep of the mirrors found.
#[derive(Debug, Default)]
pub struct Sweep {
    pub emails: BTreeMap<String, Author>,
    pub commits: usize,
    pub failed: usize,
}

/// Read every mirror, `jobs` at a time, reporting one line per repository as it
/// lands so a long run says what it is doing.
pub fn sweep(
    found: &[Repo],
    since: &str,
    until: Option<&str>,
    coauthors: bool,
    jobs: usize,
) -> Sweep {
    let width = found.len().to_string().len();
    let total = found.len();
    let next = AtomicUsize::new(0);
    let sweep = Mutex::new(Sweep::default());

    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= total {
                        return;
                    }
                    let clone = &found[index];
                    let slug = clone.slug();
                    let position = format!("[{:>width$}/{total}]", index + 1);
                    match commits_of(&clone.path, since, until, coauthors) {
                        Err(problem) => {
                            eprintln!("{position} {slug} FAILED: {problem}");
                            sweep.lock().expect("sweep").failed += 1;
                        }
                        Ok(commits) => {
                            let seen = addresses_in(clone, &commits, coauthors);
                            {
                                let mut sweep = sweep.lock().expect("sweep");
                                for (email, found) in &seen {
                                    let into =
                                        sweep.emails.entry(email.clone()).or_insert_with(|| {
                                            Author {
                                                commits: 0,
                                                name: found.name.clone(),
                                                repos: Vec::new(),
                                                sample: None,
                                            }
                                        });
                                    into.commits += found.commits;
                                    into.repos.push(slug.clone());
                                    // The lowest `org/repo@sha`, not whichever
                                    // thread arrived first: the sample ends up
                                    // in the cache as `via`, and a cache that
                                    // changes with thread scheduling is a cache
                                    // that cannot be diffed between runs.
                                    into.sample = match (&into.sample, &found.sample) {
                                        (Some(have), Some(new)) => Some(have.min(new).clone()),
                                        (Some(have), None) => Some(have.clone()),
                                        (None, candidate) => candidate.clone(),
                                    };
                                }
                                sweep.commits += commits.len();
                            }
                            println!(
                                "{position} {slug} {} commits, {} addresses",
                                commits.len(),
                                seen.len()
                            );
                        }
                    }
                }
            });
        }
    });

    let mut sweep = sweep.into_inner().expect("sweep");
    for author in sweep.emails.values_mut() {
        author.repos = crate::tac::sorted(author.repos.drain(..));
    }
    sweep
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leading_dot_becomes_an_underscore_on_disk() {
        assert_eq!(on_disk(".github"), "_github");
        assert_eq!(on_disk("fabric"), "fabric");
        assert_eq!(on_disk("a.b"), "a.b", "only a leading dot");
    }

    #[test]
    fn a_coauthor_trailer_gives_up_its_address_and_name() {
        assert_eq!(
            parse_trailer("Ry Jones <ry@example.com>"),
            Some(("ry@example.com".into(), "Ry Jones".into()))
        );
        assert_eq!(
            parse_trailer("  UPPER <A@B.COM> "),
            Some(("a@b.com".into(), "UPPER".into())),
            "addresses are compared lowercased"
        );
        assert_eq!(parse_trailer("no address here"), None);
        assert_eq!(parse_trailer("<>"), None);
    }

    fn commit(sha: &str, email: &str, name: &str, coauthors: &[&str]) -> Commit {
        Commit {
            sha: sha.into(),
            email: email.into(),
            name: name.into(),
            coauthors: coauthors.iter().map(|c| c.to_string()).collect(),
        }
    }

    fn repo_of(org: &str, name: &str) -> Repo {
        Repo {
            org: org.into(),
            name: name.into(),
            path: PathBuf::from("/nowhere"),
        }
    }

    #[test]
    fn an_address_carries_its_count_and_one_sample_commit() {
        let clone = repo_of("acme", "widget");
        let commits = vec![
            commit("aaa", "ry@example.com", "Ry", &[]),
            commit("bbb", "ry@example.com", "Ry", &[]),
        ];
        let seen = addresses_in(&clone, &commits, false);
        assert_eq!(seen["ry@example.com"].commits, 2);
        assert_eq!(
            seen["ry@example.com"].sample.as_deref(),
            Some("acme/widget@aaa"),
            "the first commit seen is the one the API is asked about"
        );
    }

    #[test]
    fn a_bot_address_is_not_a_contributor() {
        let clone = repo_of("acme", "widget");
        let commits = vec![
            commit("aaa", "noreply@github.com", "GitHub", &[]),
            commit(
                "bbb",
                "41898282+github-actions[bot]@users.noreply.github.com",
                "bot",
                &[],
            ),
            commit("ccc", "ry@example.com", "Ry", &[]),
        ];
        let seen = addresses_in(&clone, &commits, false);
        assert_eq!(seen.keys().collect::<Vec<_>>(), ["ry@example.com"]);
    }

    #[test]
    fn coauthors_count_only_when_asked_for_and_bring_no_sample() {
        let clone = repo_of("acme", "widget");
        let commits = vec![commit(
            "aaa",
            "ry@example.com",
            "Ry",
            &["Pat <pat@example.com>"],
        )];

        let without = addresses_in(&clone, &commits, false);
        assert_eq!(without.len(), 1, "the trailer is ignored by default");

        let with = addresses_in(&clone, &commits, true);
        assert_eq!(with["pat@example.com"].commits, 1);
        assert_eq!(
            with["pat@example.com"].sample, None,
            "a co-author has no commit of their own to look up"
        );
    }
}
