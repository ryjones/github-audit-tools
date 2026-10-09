//! Turning commit addresses into GitHub accounts.
//!
//! A commit names an address; eligibility is about accounts. GitHub will say
//! which account a commit belongs to -- `GET /repos/{org}/{repo}/commits/{sha}`
//! answers with `author.login` -- so one call per address, never per commit,
//! and the answer is cached by address alone.
//!
//! The cache is the point. It is keyed by address and nothing else, so it is
//! worth keeping across enterprises and across years: every address already in
//! it is a call the next run does not make.
//!
//! An address no account claims is no contribution as GitHub counts it either,
//! so it is left out rather than guessed at -- and listed, busiest first, so a
//! real person who has simply never linked that address can be spotted and
//! mapped by hand.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::tac::{self, Window, is_bot};
use crate::tac_authors::Author;

/// One cached answer about one address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resolved {
    /// The account, or `null` for an address GitHub links to nobody.
    pub login: Option<String>,
    /// `api` when GitHub named the account, `noreply` when the address itself
    /// did.
    pub source: Option<String>,
    /// The `org/repo@sha` that was asked about.
    pub via: Option<String>,
    pub resolved: String,
}

/// `cache.json`: every address ever looked up, by any run.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CacheFile {
    pub emails: BTreeMap<String, Resolved>,
    #[serde(default)]
    pub updated: String,
}

/// The cache, with the rows this run added kept apart so a save merges rather
/// than overwrites: another run of this tool may have added rows in the
/// meantime, and no run should cost another one its lookups.
pub struct Cache {
    path: PathBuf,
    known: BTreeMap<String, Resolved>,
    added: BTreeMap<String, Resolved>,
}

impl Cache {
    pub fn open(path: &Path) -> Self {
        let known = tac::read_json::<CacheFile>(path)
            .map(|file| file.emails)
            .unwrap_or_default();
        Cache {
            path: path.to_path_buf(),
            known,
            added: BTreeMap::new(),
        }
    }

    pub fn get(&self, email: &str) -> Option<&Resolved> {
        self.known.get(email)
    }

    pub fn len(&self) -> usize {
        self.known.len()
    }

    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }

    /// Remember an answer, saving every 25 so an interrupted run keeps most of
    /// what it paid for.
    pub fn put(&mut self, email: &str, entry: Resolved) -> Result<()> {
        self.known.insert(email.to_string(), entry.clone());
        self.added.insert(email.to_string(), entry);
        if self.added.len().is_multiple_of(25) {
            self.save()?;
        }
        Ok(())
    }

    /// Re-read what is on disk, merge this run's rows over it, and write.
    pub fn save(&mut self) -> Result<()> {
        let mut on_disk = tac::read_json::<CacheFile>(&self.path)
            .map(|file| file.emails)
            .unwrap_or_default();
        for (email, entry) in &self.added {
            on_disk.insert(email.clone(), entry.clone());
        }
        for (email, entry) in &on_disk {
            self.known.insert(email.clone(), entry.clone());
        }
        tac::write_json(
            &self.path,
            &CacheFile {
                emails: on_disk,
                updated: tac::today(),
            },
        )
    }
}

/// One account, and everything in the window that was theirs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub login: String,
    pub commits: usize,
    pub emails: Vec<String>,
    pub repos: Vec<String>,
}

/// An address with no account, kept so it can be looked at.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Unresolved {
    pub email: String,
    pub name: Option<String>,
    pub commits: usize,
    /// The first few repositories it committed to, enough to recognize.
    pub repos: Vec<String>,
}

/// `logins.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Logins {
    pub window: Window,
    /// Keyed by lowercased login.
    pub logins: BTreeMap<String, Account>,
    pub bots_dropped: Vec<String>,
    /// Busiest first: the ones worth mapping by hand are at the top.
    pub unresolved: Vec<Unresolved>,
}

/// The account a `@users.noreply.github.com` address names.
///
/// GitHub hands these out in two shapes, `login@` and `12345+login@`, and both
/// name the account outright -- which is the only thing that can be read off an
/// address without asking.
pub fn login_from_noreply(email: &str) -> Option<String> {
    let local = email.strip_suffix("@users.noreply.github.com")?;
    let login = match local.split_once('+') {
        Some((digits, login))
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) =>
        {
            login
        }
        Some(_) => return None,
        None => local,
    };
    let valid = !login.is_empty()
        && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !login.starts_with('-')
        && !login.ends_with('-');
    valid.then(|| login.to_string())
}

/// Hand-written mappings for addresses no account claims. They are never looked
/// up and they override whatever the API said.
pub fn aliases(path: &Path) -> BTreeMap<String, String> {
    tac::read_json::<BTreeMap<String, String>>(path)
        .unwrap_or_default()
        .into_iter()
        .map(|(email, login)| (email.trim().to_lowercase(), login.trim().to_string()))
        .collect()
}

/// Which addresses still need asking about: not aliased, not already answered,
/// or answered with nothing when the run wants those retried.
pub fn pending(
    emails: &BTreeMap<String, Author>,
    cache: &Cache,
    aliases: &BTreeMap<String, String>,
    retry_unresolved: bool,
) -> Vec<String> {
    emails
        .keys()
        .filter(|email| {
            if aliases.contains_key(*email) {
                return false;
            }
            match cache.get(email) {
                None => true,
                Some(hit) => retry_unresolved && hit.login.is_none(),
            }
        })
        .cloned()
        .collect()
}

/// Fold the addresses into accounts: who they are, how much they did, and which
/// addresses turned out to be the same person.
pub fn accounts(
    emails: &BTreeMap<String, Author>,
    cache: &Cache,
    aliases: &BTreeMap<String, String>,
) -> (BTreeMap<String, Account>, Vec<String>, Vec<Unresolved>) {
    let mut logins: BTreeMap<String, Account> = BTreeMap::new();
    let mut unresolved = Vec::new();

    for (email, author) in emails {
        let login = aliases
            .get(email)
            .cloned()
            .or_else(|| cache.get(email).and_then(|hit| hit.login.clone()));
        let Some(login) = login.filter(|login| !login.is_empty()) else {
            unresolved.push(Unresolved {
                email: email.clone(),
                name: Some(author.name.clone()).filter(|name| !name.is_empty()),
                commits: author.commits,
                repos: author.repos.iter().take(5).cloned().collect(),
            });
            continue;
        };
        let entry = logins
            .entry(login.to_lowercase())
            .or_insert_with(|| Account {
                login,
                commits: 0,
                emails: Vec::new(),
                repos: Vec::new(),
            });
        entry.commits += author.commits;
        entry.emails.push(email.clone());
        entry.repos.extend(author.repos.iter().cloned());
    }

    for account in logins.values_mut() {
        account.emails = tac::sorted(account.emails.drain(..));
        account.repos = tac::sorted(account.repos.drain(..));
    }

    let bots: Vec<String> = logins
        .keys()
        .filter(|login| is_bot(login))
        .cloned()
        .collect();
    for bot in &bots {
        logins.remove(bot);
    }

    // Busiest first, then by address so a run is reproducible.
    unresolved.sort_by(|a, b| {
        b.commits
            .cmp(&a.commits)
            .then_with(|| a.email.cmp(&b.email))
    });
    (logins, bots, unresolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn author(commits: usize, repos: &[&str]) -> Author {
        Author {
            commits,
            name: "Someone".into(),
            repos: repos.iter().map(|r| r.to_string()).collect(),
            sample: None,
        }
    }

    fn resolved(login: Option<&str>) -> Resolved {
        Resolved {
            login: login.map(str::to_string),
            source: Some("api".into()),
            via: None,
            resolved: "2026-10-09".into(),
        }
    }

    #[test]
    fn a_noreply_address_names_its_own_account() {
        assert_eq!(
            login_from_noreply("ryjones@users.noreply.github.com").as_deref(),
            Some("ryjones")
        );
        assert_eq!(
            login_from_noreply("12345+RyJones@users.noreply.github.com").as_deref(),
            Some("RyJones")
        );
        for other in [
            "ry@example.com",
            "ry@users.noreply.gitlab.com",
            "+ry@users.noreply.github.com",
            "a+b+c@users.noreply.github.com",
            "-ry@users.noreply.github.com",
            "@users.noreply.github.com",
        ] {
            assert_eq!(login_from_noreply(other), None, "{other}");
        }
    }

    fn cache_with(rows: &[(&str, Option<&str>)]) -> Cache {
        let mut cache = Cache::open(Path::new("/nonexistent/cache.json"));
        for (email, login) in rows {
            cache.known.insert((*email).to_string(), resolved(*login));
        }
        cache
    }

    #[test]
    fn only_the_unanswered_addresses_are_looked_up() {
        let emails: BTreeMap<String, Author> = [
            ("known@x.com".to_string(), author(1, &[])),
            ("new@x.com".to_string(), author(1, &[])),
            ("nobody@x.com".to_string(), author(1, &[])),
            ("aliased@x.com".to_string(), author(1, &[])),
        ]
        .into_iter()
        .collect();
        let cache = cache_with(&[("known@x.com", Some("known")), ("nobody@x.com", None)]);
        let aliases: BTreeMap<String, String> =
            [("aliased@x.com".to_string(), "mapped".to_string())]
                .into_iter()
                .collect();

        assert_eq!(pending(&emails, &cache, &aliases, false), ["new@x.com"]);
        assert_eq!(
            pending(&emails, &cache, &aliases, true),
            ["new@x.com", "nobody@x.com"],
            "--retry-unresolved asks again about the empty answers"
        );
    }

    #[test]
    fn two_addresses_for_one_account_become_one_person() {
        let emails: BTreeMap<String, Author> = [
            ("work@x.com".to_string(), author(3, &["acme/widget"])),
            ("home@x.com".to_string(), author(2, &["acme/gadget"])),
        ]
        .into_iter()
        .collect();
        let cache = cache_with(&[
            ("work@x.com", Some("RyJones")),
            ("home@x.com", Some("ryjones")),
        ]);
        let (logins, _, unresolved) = accounts(&emails, &cache, &BTreeMap::new());

        assert_eq!(logins.len(), 1);
        let account = &logins["ryjones"];
        assert_eq!(account.commits, 5);
        assert_eq!(account.emails, ["home@x.com", "work@x.com"]);
        assert_eq!(account.repos, ["acme/gadget", "acme/widget"]);
        assert!(unresolved.is_empty());
    }

    #[test]
    fn an_alias_wins_over_the_api_and_over_nothing() {
        let emails: BTreeMap<String, Author> = [
            ("ry@linux.com".to_string(), author(9, &[])),
            ("wrong@x.com".to_string(), author(1, &[])),
        ]
        .into_iter()
        .collect();
        let cache = cache_with(&[("ry@linux.com", None), ("wrong@x.com", Some("stale"))]);
        let aliases: BTreeMap<String, String> = [
            ("ry@linux.com".to_string(), "ryjones".to_string()),
            ("wrong@x.com".to_string(), "right".to_string()),
        ]
        .into_iter()
        .collect();
        let (logins, _, unresolved) = accounts(&emails, &cache, &aliases);
        assert_eq!(logins["ryjones"].commits, 9);
        assert_eq!(logins["right"].commits, 1, "the alias overrides the cache");
        assert!(unresolved.is_empty());
    }

    #[test]
    fn an_address_with_no_account_is_reported_busiest_first() {
        let emails: BTreeMap<String, Author> = [
            ("quiet@x.com".to_string(), author(1, &["a/b"])),
            (
                "busy@x.com".to_string(),
                author(40, &["a/b", "c/d", "e/f", "g/h", "i/j", "k/l"]),
            ),
        ]
        .into_iter()
        .collect();
        let cache = cache_with(&[]);
        let (logins, _, unresolved) = accounts(&emails, &cache, &BTreeMap::new());
        assert!(logins.is_empty());
        assert_eq!(unresolved[0].email, "busy@x.com");
        assert_eq!(unresolved[0].repos.len(), 5, "enough to recognize, not all");
        assert_eq!(unresolved[1].email, "quiet@x.com");
    }

    #[test]
    fn a_bot_account_is_dropped_from_the_nominees() {
        let emails: BTreeMap<String, Author> = [
            ("bot@x.com".to_string(), author(100, &[])),
            ("ry@x.com".to_string(), author(1, &[])),
        ]
        .into_iter()
        .collect();
        let cache = cache_with(&[
            ("bot@x.com", Some("dependabot[bot]")),
            ("ry@x.com", Some("ryjones")),
        ]);
        let (logins, bots, _) = accounts(&emails, &cache, &BTreeMap::new());
        assert_eq!(bots, ["dependabot[bot]"]);
        assert_eq!(logins.keys().collect::<Vec<_>>(), ["ryjones"]);
    }
}
