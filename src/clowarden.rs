//! Reading the CLOWarden configs that `mirror-clowarden.zsh` clones.
//!
//! CLOWarden applies one YAML file per organization, so that file is the
//! organization's declared access. The same schema answers to five names, and
//! the files in the wild are loose about types: a login may be quoted, bare, or
//! parse as a number (`007`), and a key the schema does not mention is simply
//! present. So this walks the YAML rather than deserializing into a struct, and
//! coerces scalars to strings the way the Python it replaces did.
//!
//! Nothing here touches the network: a mirror is a directory of checkouts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_yaml_ng::Value;

/// One schema, several filenames, in the order the config repositories use
/// them. `clowarden.yaml` is the odd one out, used by `KapunSDK/tsc`; without
/// it that organization reads as declaring nothing.
pub const CONFIG_NAMES: [&str; 7] = [
    "config.yaml",
    "config.yml",
    "teams.yml",
    "teams.yaml",
    "access-control.yaml",
    "clowarden.yaml",
    "clowarden.yml",
];

/// "Write access or higher" in CLOWarden's vocabulary. `read` and `triage` are
/// not it.
pub const WRITE_PLUS: [&str; 3] = ["write", "maintain", "admin"];

/// A team as a config declares it. Maintainers and members are kept apart
/// because a role mismatch between the config and GitHub is itself a finding.
#[derive(Debug, Default, Clone)]
pub struct Team {
    pub name: String,
    pub slug: String,
    pub maintainers: Vec<String>,
    pub members: Vec<String>,
}

/// A repository as a config declares it: which teams reach it, with what
/// permission, and any collaborator named straight on the repository.
#[derive(Debug, Default, Clone)]
pub struct Repository {
    pub name: String,
    /// team name as written in the config -> permission
    pub teams: BTreeMap<String, String>,
    /// login -> permission
    pub collaborators: BTreeMap<String, String>,
}

/// One organization's declared access, as read from its config file.
#[derive(Debug, Clone)]
pub struct Config {
    pub org: String,
    pub path: PathBuf,
    pub teams: Vec<Team>,
    pub repositories: Vec<Repository>,
}

/// What a walk of the mirror found: the configs that parsed, and the
/// organizations that had none or whose config would not parse. An organization
/// with no config has no declared intent, which is not the same as declaring
/// nothing — callers keep the two apart.
#[derive(Debug, Default)]
pub struct Mirror {
    pub configs: Vec<Config>,
    pub skipped: Vec<String>,
}

impl Mirror {
    /// Organization logins that had a readable config, in the order found.
    pub fn orgs(&self) -> Vec<String> {
        self.configs.iter().map(|c| c.org.clone()).collect()
    }

    /// Teams holding write or better on at least one repository, as
    /// `(org, team_slug)`. A collaborator named on a repository becomes the
    /// pseudo-team `collaborator:<repo>`, so one vocabulary covers both.
    pub fn write_teams(&self) -> std::collections::BTreeSet<(String, String)> {
        let mut teams = std::collections::BTreeSet::new();
        for config in &self.configs {
            for repo in &config.repositories {
                for (team, permission) in &repo.teams {
                    if is_write_plus(permission) {
                        teams.insert((config.org.clone(), team_slug(team)));
                    }
                }
                for permission in repo.collaborators.values() {
                    if is_write_plus(permission) {
                        teams.insert((config.org.clone(), format!("collaborator:{}", repo.name)));
                    }
                }
            }
        }
        teams
    }

    /// Everyone a config puts on a team, as `(org, team_slug) -> logins`, plus
    /// one entry per repository that names collaborators directly.
    pub fn rosters(&self) -> BTreeMap<(String, String), Vec<String>> {
        let mut rosters: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for config in &self.configs {
            for team in &config.teams {
                let mut people = team.maintainers.clone();
                people.extend(team.members.iter().cloned());
                rosters.insert((config.org.clone(), team.slug.clone()), people);
            }
            for repo in &config.repositories {
                for (login, permission) in &repo.collaborators {
                    if is_write_plus(permission) {
                        rosters
                            .entry((config.org.clone(), format!("collaborator:{}", repo.name)))
                            .or_default()
                            .push(login.clone());
                    }
                }
            }
        }
        rosters
    }
}

pub fn is_write_plus(permission: &str) -> bool {
    WRITE_PLUS.contains(&permission.to_lowercase().as_str())
}

/// GitHub's team slug, near enough: lowercase, and every run of anything else
/// becomes a single dash, with no dash at either end.
pub fn team_slug(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

/// Walk `mirror/<org>/<repo>` and read the config each organization declares.
///
/// `warn` is called with one line per organization whose config could not be
/// read, so the caller decides where that goes.
pub fn read(mirror: &Path, mut warn: impl FnMut(String)) -> Result<Mirror> {
    let mut found = Mirror::default();
    for org_dir in sorted_dirs(mirror)? {
        let org = file_name(&org_dir);
        for repo_dir in sorted_dirs(&org_dir)? {
            if !repo_dir.join(".git").exists() {
                continue;
            }
            let Some(path) = CONFIG_NAMES
                .iter()
                .map(|name| repo_dir.join(name))
                .find(|path| path.exists())
            else {
                found.skipped.push(org.clone());
                continue;
            };
            match parse(&path, &org) {
                Ok(config) => found.configs.push(config),
                Err(problem) => {
                    warn(format!(
                        "warning: cannot parse {}: {problem}",
                        path.display()
                    ));
                    found.skipped.push(org.clone());
                }
            }
        }
    }
    Ok(found)
}

/// Parse one config file. Unknown keys are ignored, and a team without a name
/// is dropped: it cannot be matched to anything on GitHub.
pub fn parse(path: &Path, org: &str) -> Result<Config> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let doc: Value =
        serde_yaml_ng::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;

    let mut config = Config {
        org: org.to_string(),
        path: path.to_path_buf(),
        teams: Vec::new(),
        repositories: Vec::new(),
    };

    for team in sequence(&doc, "teams") {
        let Some(name) = scalar(team.get("name")) else {
            continue;
        };
        if name.trim().is_empty() {
            continue;
        }
        config.teams.push(Team {
            slug: team_slug(&name),
            name,
            maintainers: logins(team.get("maintainers")),
            members: logins(team.get("members")),
        });
    }

    for repo in sequence(&doc, "repositories") {
        config.repositories.push(Repository {
            name: scalar(repo.get("name")).unwrap_or_default(),
            teams: mapping(repo.get("teams")),
            collaborators: mapping(repo.get("collaborators")),
        });
    }

    Ok(config)
}

// --------------------------------------------------------------- YAML helpers

/// Directories directly under `path`, sorted, so a run is reproducible.
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

/// `doc[key]` as a sequence, or empty when it is missing, null, or not a list.
fn sequence<'a>(doc: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    doc.get(key)
        .and_then(Value::as_sequence)
        .map(|items| items.iter())
        .unwrap_or_default()
}

/// A YAML scalar as the string a config author meant. `007` parses as a number
/// and `true` as a boolean; both are logins or names to us.
fn scalar(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// A list of logins, skipping anything that is not a scalar or is blank.
fn logins(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_sequence)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| scalar(Some(item)))
                .map(|login| login.trim().to_string())
                .filter(|login| !login.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// A `key: value` block of scalars, e.g. a repository's `teams:` permissions.
fn mapping(value: Option<&Value>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(Value::Mapping(map)) = value {
        for (key, value) in map {
            if let (Some(key), Some(value)) = (scalar(Some(key)), scalar(Some(value))) {
                out.insert(key, value);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_team_slug_collapses_every_run_of_punctuation() {
        assert_eq!(team_slug("Fabric Maintainers"), "fabric-maintainers");
        assert_eq!(team_slug("TSC / Chairs"), "tsc-chairs");
        assert_eq!(
            team_slug("--leading and trailing--"),
            "leading-and-trailing"
        );
        assert_eq!(team_slug("Besu"), "besu");
        assert_eq!(team_slug("a.b_c"), "a-b-c");
    }

    #[test]
    fn write_or_better_is_three_permissions_and_not_the_others() {
        for yes in ["write", "maintain", "admin", "Admin", "WRITE"] {
            assert!(is_write_plus(yes), "{yes} should count");
        }
        for no in ["read", "triage", "", "none"] {
            assert!(!is_write_plus(no), "{no} should not count");
        }
    }

    /// Write one config to its own directory, since the tests run in parallel
    /// and the filename is part of what `parse` is given.
    fn config_from(case: &str, yaml: &str) -> Config {
        let dir =
            std::env::temp_dir().join(format!("clowarden-test-{}-{case}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.yaml");
        std::fs::write(&path, yaml).expect("write config");
        let config = parse(&path, "acme").expect("parses");
        std::fs::remove_dir_all(&dir).ok();
        config
    }

    #[test]
    fn a_config_yields_teams_rosters_and_the_teams_with_write() {
        let config = config_from(
            "full",
            r#"
teams:
  - name: Maintainers
    maintainers:
      - alice
    members:
      - bob
      - 007
  - name: Readers
    members: [carol]
  - maintainers: [nobody]
repositories:
  - name: widget
    teams:
      Maintainers: maintain
      Readers: read
  - name: gadget
    teams:
      Readers: write
    collaborators:
      dave: admin
      eve: read
"#,
        );
        assert_eq!(config.teams.len(), 2, "the nameless team is dropped");
        assert_eq!(config.teams[0].maintainers, ["alice"]);
        assert_eq!(config.teams[0].members, ["bob", "007"]);

        let mirror = Mirror {
            configs: vec![config],
            skipped: Vec::new(),
        };
        let write = mirror.write_teams();
        assert!(write.contains(&("acme".into(), "maintainers".into())));
        assert!(
            write.contains(&("acme".into(), "readers".into())),
            "readers hold write on gadget, even though only read on widget"
        );
        assert!(write.contains(&("acme".into(), "collaborator:gadget".into())));

        let rosters = mirror.rosters();
        assert_eq!(
            rosters[&("acme".to_string(), "maintainers".to_string())],
            ["alice", "bob", "007"]
        );
        assert_eq!(
            rosters[&("acme".to_string(), "collaborator:gadget".to_string())],
            ["dave"],
            "eve is only a reader, so she is not on the roster"
        );
    }

    #[test]
    fn a_config_with_no_teams_or_repositories_reads_as_empty_not_as_an_error() {
        let config = config_from("empty", "legacy: true\n");
        assert!(config.teams.is_empty());
        assert!(config.repositories.is_empty());
    }

    #[test]
    fn a_mirror_reads_every_spelling_and_names_the_orgs_with_none() {
        let root = std::env::temp_dir().join(format!("clowarden-mirror-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let checkout = |org: &str, repo: &str, file: Option<&str>| {
            let dir = root.join(org).join(repo);
            std::fs::create_dir_all(dir.join(".git")).expect("checkout");
            if let Some(file) = file {
                std::fs::write(
                    dir.join(file),
                    "teams:\n  - name: Maintainers\n    members: [alice]\n\
                     repositories:\n  - name: widget\n    teams: {Maintainers: write}\n",
                )
                .expect("write config");
            }
        };
        // KapunSDK's spelling, the enterprise's usual one, and an org whose
        // config repository holds no config at all.
        checkout("KapunSDK", "tsc", Some("clowarden.yaml"));
        checkout("acme", "governance", Some("config.yaml"));
        checkout("quiet-org", "governance", None);

        let mut warnings = Vec::new();
        let mirror = read(&root, |warning| warnings.push(warning)).expect("reads");
        std::fs::remove_dir_all(&root).ok();

        assert_eq!(mirror.orgs(), ["KapunSDK", "acme"]);
        assert_eq!(mirror.skipped, ["quiet-org"]);
        assert!(warnings.is_empty(), "a missing config is not a warning");
        assert_eq!(
            mirror.rosters()[&("KapunSDK".to_string(), "maintainers".to_string())],
            ["alice"],
            "clowarden.yaml is read like the rest"
        );
    }
}
