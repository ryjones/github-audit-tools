//! The latest commit in each repository an access-control config declares.
//!
//! The voters phase knows which repositories have access control — a CLOWarden
//! config names them, with a permission per team and per collaborator — but not
//! whether anything still happens in them. A team holding write on a repository
//! nobody has touched in two years still makes voters, so for every repository
//! a config declares this records the newest commit in its mirror and how many
//! commits fall in the election window, next to the access that was declared.
//!
//! No network: the mirrors are the bare clones `clone-repos.zsh` made. A
//! declared repository may have no mirror at all — it is archived, private,
//! renamed, or simply not cloned — and that is said with `mirrored: false`
//! rather than counted as silence.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::clowarden::{Mirror, is_write_plus};
use crate::tac_authors::{self, on_disk};

/// One repository a config declares access on, and what has happened in it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoAccess {
    /// Team name as the config writes it -> permission.
    pub teams: BTreeMap<String, String>,
    /// Login -> permission, for a collaborator named on the repository itself.
    pub collaborators: BTreeMap<String, String>,
    /// Whether any of that access is write, maintain or admin: only those make
    /// voters.
    pub write: bool,
    /// Whether a bare clone of it is under `--repos`.
    pub mirrored: bool,
    /// Committer date of the newest commit on any ref, RFC 3339. `null` when
    /// there is no mirror, or the mirror has no commits.
    pub latest_commit: Option<String>,
    /// Commits in the election window, counted the way the authors phase counts
    /// them. Zero against a `latest_commit` of years ago is a dormant
    /// repository whose teams still vote.
    pub commits_in_window: usize,
}

/// Every repository the configs declare, as `org/repo` -> declared access.
///
/// One organization can have more than one config repository in the mirror, and
/// two configs can name the same repository; the access they declare is merged
/// rather than one overwriting the other.
pub fn declared(mirror: &Mirror) -> BTreeMap<String, RepoAccess> {
    let mut repos: BTreeMap<String, RepoAccess> = BTreeMap::new();
    for config in &mirror.configs {
        for repo in &config.repositories {
            let entry = repos
                .entry(format!("{}/{}", config.org, repo.name))
                .or_default();
            for (team, permission) in &repo.teams {
                entry.write |= is_write_plus(permission);
                entry.teams.insert(team.clone(), permission.clone());
            }
            for (login, permission) in &repo.collaborators {
                entry.write |= is_write_plus(permission);
                entry
                    .collaborators
                    .insert(login.clone(), permission.clone());
            }
        }
    }
    repos
}

/// `clone-repos.zsh` spells a repository on disk with a leading dot replaced,
/// and the configs do not agree with GitHub about case, so the mirrors are
/// indexed by lowercased `org/name`.
fn index(repos: &Path) -> Result<BTreeMap<String, PathBuf>> {
    if !repos.is_dir() {
        return Ok(BTreeMap::new());
    }
    Ok(tac_authors::mirrors(repos, None)?
        .into_iter()
        .map(|clone| (clone.slug().to_lowercase(), clone.path))
        .collect())
}

/// Read the newest commit and the window count for every declared repository,
/// `jobs` mirrors at a time.
///
/// `warn` is called once per mirror that could not be read, so the caller
/// decides where that goes; those keep `mirrored: true` and no date, because
/// the clone is there and the question went unanswered.
pub fn fill(
    repos: &mut BTreeMap<String, RepoAccess>,
    mirrors: &Path,
    since: &str,
    until: Option<&str>,
    jobs: usize,
    mut warn: impl FnMut(String),
) -> Result<()> {
    let index = index(mirrors)?;

    // Which declared repositories have a clone, paired with it. Collected first
    // so the threads below only read.
    let mut work: Vec<(String, PathBuf)> = Vec::new();
    for (slug, access) in repos.iter_mut() {
        let (org, name) = slug.split_once('/').unwrap_or((slug.as_str(), ""));
        let key = format!("{org}/{}", on_disk(name)).to_lowercase();
        match index.get(&key) {
            Some(path) => {
                access.mirrored = true;
                work.push((slug.clone(), path.clone()));
            }
            None => access.mirrored = false,
        }
    }

    let total = work.len();
    let next = AtomicUsize::new(0);
    let found: Mutex<BTreeMap<String, (Option<String>, usize)>> = Mutex::new(BTreeMap::new());
    let problems: Mutex<Vec<String>> = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let position = next.fetch_add(1, Ordering::Relaxed);
                    if position >= total {
                        return;
                    }
                    let (slug, path) = &work[position];
                    match activity_of(path, since, until) {
                        Ok(activity) => {
                            found
                                .lock()
                                .expect("activity")
                                .insert(slug.clone(), activity);
                        }
                        Err(problem) => problems
                            .lock()
                            .expect("problems")
                            .push(format!("warning: {slug}: {problem}")),
                    }
                }
            });
        }
    });

    for (slug, (latest, commits)) in found.into_inner().expect("activity") {
        if let Some(access) = repos.get_mut(&slug) {
            access.latest_commit = latest;
            access.commits_in_window = commits;
        }
    }
    let mut problems = problems.into_inner().expect("problems");
    problems.sort();
    for problem in problems {
        warn(problem);
    }
    Ok(())
}

/// `(newest commit date, commits in the window)` for one bare mirror.
///
/// Two cheap traversals rather than one: the newest commit on any ref answers
/// "is this repository alive at all", and the window count answers "did it
/// count towards this election". A mirror with no refs at all is not an error —
/// it reads as no commits.
fn activity_of(path: &Path, since: &str, until: Option<&str>) -> Result<(Option<String>, usize)> {
    let latest = git(path, &["log", "--all", "--no-notes", "-1", "--format=%cI"])?;
    if latest.is_none() {
        return Ok((None, 0));
    }

    let mut args = vec![
        "rev-list".to_string(),
        "--all".to_string(),
        "--count".to_string(),
        format!("--since={since}"),
    ];
    if let Some(until) = until {
        args.push(format!("--until={until}"));
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let commits = git(path, &args)?
        .and_then(|count| count.parse::<usize>().ok())
        .unwrap_or(0);
    Ok((latest, commits))
}

/// One git command in a bare mirror, trimmed, `None` when it printed nothing.
///
/// `GIT_NO_LAZY_FETCH` keeps a partial clone from reaching for trees it was
/// deliberately cloned without, and `GIT_TERMINAL_PROMPT` keeps a repository
/// that has gone private from stopping to ask for a password — the same two the
/// authors phase sets. A mirror with no refs makes git exit non-zero on a
/// traversal; that is silence, not a failure.
fn git(path: &Path, args: &[&str]) -> Result<Option<String>> {
    let done = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .with_context(|| format!("running git {} in {}", args[0], path.display()))?;

    let stdout = String::from_utf8_lossy(&done.stdout);
    let first = stdout.lines().next().unwrap_or_default().trim().to_string();
    if !done.status.success() {
        let stderr = String::from_utf8_lossy(&done.stderr);
        let last = stderr.trim().lines().last().unwrap_or_default();
        if no_refs(last) {
            return Ok(None);
        }
        bail!(
            "{}",
            if last.is_empty() {
                format!("git {} failed", args[0])
            } else {
                last.to_string()
            }
        );
    }
    Ok(if first.is_empty() { None } else { Some(first) })
}

/// What git says about a clone that holds no commits yet. Wording differs by
/// version, so this matches on the parts that have not moved.
fn no_refs(stderr: &str) -> bool {
    let line = stderr.to_lowercase();
    line.contains("does not have any commits yet")
        || line.contains("bad default revision")
        || line.contains("ambiguous argument 'head'")
}

/// How the declared repositories divide up, for the line the phase prints.
///
/// `write_quiet` and `write_unmirrored` are kept apart because they are not the
/// same claim: one is a repository that was read and had nothing in it, the
/// other is one that was never read.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub declared: usize,
    pub write: usize,
    /// Write or better, mirrored, and no commit in the window: the teams on
    /// these still make voters.
    pub write_quiet: usize,
    /// Write or better with no clone under `--repos`, so nothing was read
    /// about them either way.
    pub write_unmirrored: usize,
    /// Every declared repository with no clone, whatever the permission.
    pub unmirrored: usize,
}

pub fn summarize(repos: &BTreeMap<String, RepoAccess>) -> Summary {
    let mut summary = Summary {
        declared: repos.len(),
        ..Summary::default()
    };
    for access in repos.values() {
        if access.write {
            summary.write += 1;
            if !access.mirrored {
                summary.write_unmirrored += 1;
            } else if access.commits_in_window == 0 {
                summary.write_quiet += 1;
            }
        }
        if !access.mirrored {
            summary.unmirrored += 1;
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clowarden::{Config, Repository};

    fn repository(
        name: &str,
        teams: &[(&str, &str)],
        collaborators: &[(&str, &str)],
    ) -> Repository {
        Repository {
            name: name.into(),
            teams: teams
                .iter()
                .map(|(team, permission)| (team.to_string(), permission.to_string()))
                .collect(),
            collaborators: collaborators
                .iter()
                .map(|(login, permission)| (login.to_string(), permission.to_string()))
                .collect(),
        }
    }

    fn mirror(configs: Vec<Config>) -> Mirror {
        Mirror {
            configs,
            skipped: Vec::new(),
        }
    }

    fn config(org: &str, repositories: Vec<Repository>) -> Config {
        Config {
            org: org.into(),
            path: "config.yaml".into(),
            teams: Vec::new(),
            repositories,
        }
    }

    #[test]
    fn every_declared_repository_is_listed_with_the_access_it_declares() {
        let declared = declared(&mirror(vec![config(
            "acme",
            vec![
                repository("widget", &[("Maintainers", "maintain")], &[]),
                repository("docs", &[("Readers", "read")], &[]),
            ],
        )]));
        assert_eq!(
            declared.keys().collect::<Vec<_>>(),
            ["acme/docs", "acme/widget"]
        );
        assert!(declared["acme/widget"].write);
        assert_eq!(declared["acme/widget"].teams["Maintainers"], "maintain");
        assert!(
            !declared["acme/docs"].write,
            "read is access, but not the kind that votes"
        );
    }

    #[test]
    fn a_collaborator_with_write_counts_as_write_on_the_repository() {
        let declared = declared(&mirror(vec![config(
            "acme",
            vec![repository("widget", &[], &[("ry", "admin")])],
        )]));
        assert!(declared["acme/widget"].write);
        assert_eq!(declared["acme/widget"].collaborators["ry"], "admin");
    }

    #[test]
    fn two_configs_naming_one_repository_merge_their_access() {
        let declared = declared(&mirror(vec![
            config(
                "acme",
                vec![repository("widget", &[("Maintainers", "maintain")], &[])],
            ),
            config("acme", vec![repository("widget", &[("Bots", "read")], &[])]),
        ]));
        assert_eq!(declared.len(), 1);
        assert_eq!(
            declared["acme/widget"].teams.keys().collect::<Vec<_>>(),
            ["Bots", "Maintainers"]
        );
        assert!(
            declared["acme/widget"].write,
            "the first config still holds"
        );
    }

    /// A real mirror, so the git calls are the ones a run makes.
    #[test]
    fn a_mirrored_repository_gets_its_newest_commit_and_window_count() {
        let dir = std::env::temp_dir().join(format!("tac-activity-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let work = dir.join("work");
        std::fs::create_dir_all(&work).expect("temp dir");

        let git = |args: &[&str], at: &Path| {
            let done = std::process::Command::new("git")
                .arg("-C")
                .arg(at)
                .args(args)
                .env("GIT_AUTHOR_DATE", "2024-05-05T00:00:00Z")
                .env("GIT_COMMITTER_DATE", "2024-05-05T00:00:00Z")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .output()
                .expect("git");
            assert!(done.status.success(), "git {args:?}");
        };
        git(&["init", "-q", "-b", "main", "."], &work);
        std::fs::write(work.join("a"), "a").expect("write");
        git(&["add", "a"], &work);
        git(&["commit", "-qm", "one"], &work);
        // The mirror lives where clone-repos.zsh puts it: repos/<org>/<name>.
        let mirrors = dir.join("repos");
        std::fs::create_dir_all(mirrors.join("acme")).expect("org dir");
        let done = std::process::Command::new("git")
            .args(["clone", "-q", "--bare"])
            .arg(&work)
            .arg(mirrors.join("acme").join("widget"))
            .output()
            .expect("git clone");
        assert!(done.status.success(), "clone");

        let mut repos = declared(&mirror(vec![config(
            "acme",
            vec![
                repository("widget", &[("Maintainers", "write")], &[]),
                repository("not-cloned", &[("Maintainers", "write")], &[]),
            ],
        )]));
        fill(&mut repos, &mirrors, "2024-01-01", None, 2, |warning| {
            panic!("{warning}")
        })
        .expect("fills");

        assert!(repos["acme/widget"].mirrored);
        // git spells the zero offset `Z` or `+00:00` depending on its version,
        // so the instant is what is asserted, not the spelling.
        let latest = repos["acme/widget"].latest_commit.clone().expect("a date");
        assert!(
            latest.starts_with("2024-05-05T00:00:00"),
            "newest commit, RFC 3339: {latest}"
        );
        assert_eq!(repos["acme/widget"].commits_in_window, 1);

        assert!(!repos["acme/not-cloned"].mirrored);
        assert_eq!(repos["acme/not-cloned"].latest_commit, None);
        assert_eq!(repos["acme/not-cloned"].commits_in_window, 0);

        // The same mirror, a window that starts after the only commit.
        let mut later = declared(&mirror(vec![config(
            "acme",
            vec![repository("widget", &[("Maintainers", "write")], &[])],
        )]));
        fill(&mut later, &mirrors, "2025-01-01", None, 1, |warning| {
            panic!("{warning}")
        })
        .expect("fills");
        assert_eq!(later["acme/widget"].commits_in_window, 0);
        assert_eq!(
            later["acme/widget"].latest_commit, repos["acme/widget"].latest_commit,
            "dormant in the window, but not an empty repository"
        );
        assert_eq!(
            summarize(&later),
            Summary {
                declared: 1,
                write: 1,
                write_quiet: 1,
                write_unmirrored: 0,
                unmirrored: 0,
            }
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_leading_dot_is_matched_to_the_name_on_disk() {
        // Only the mapping is under test here; nothing is cloned, so the
        // repository reads as unmirrored and the key is what matters.
        let mut repos = declared(&mirror(vec![config(
            "acme",
            vec![repository(".github", &[("Maintainers", "write")], &[])],
        )]));
        assert!(repos.contains_key("acme/.github"), "the config's spelling");
        fill(
            &mut repos,
            Path::new("/nonexistent"),
            "2024-01-01",
            None,
            1,
            |warning| panic!("{warning}"),
        )
        .expect("an absent mirrors directory is not an error");
        assert!(!repos["acme/.github"].mirrored);
    }
}
