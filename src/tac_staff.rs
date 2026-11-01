//! Logins kept off both rolls by hand.
//!
//! The phases drop the bots they can recognize -- a login ending in `[bot]`, and
//! the handful of automation accounts that look like people. Everything else is
//! a judgement call: foundation staff who hold write access because they
//! administer the organization rather than because they maintain the project, a
//! contributor's second account, a bot nobody has seen before. Those come from a
//! file kept beside the phase files, edited by a person.
//!
//! `staff.yaml`, `staff.yml` or `staff.json` at the root of this repository,
//! whichever is there, read in that order -- and tracked in git, unlike every
//! other file these phases read. It holds public GitHub logins and a reason
//! each, nothing more, and who was excluded from an election roll is part of
//! the record of that election. YAML is worth preferring: a list of people
//! being excluded wants a comment next to each one saying why. Three shapes
//! are accepted, because all three are natural to type:
//!
//! ```yaml
//! # 1. logins, nothing more
//! - some-staffer
//! - some-bot
//! ```
//!
//! ```yaml
//! # 2. login: why, which is what the reports show
//! some-staffer: foundation staff
//! some-bot: automation, not a person
//! ```
//!
//! ```yaml
//! # 3. grouped, where the group name becomes the reason
//! staff:
//!   - some-staffer
//! bots:
//!   - some-bot
//! ```
//!
//! Matching is on the lowercased login, which is how GitHub compares them, so
//! the spelling in the file does not have to match the spelling in a config.
//!
//! `--seed-staff` writes the first cut of that file: everyone a CLOWarden config
//! or GitHub itself puts on a team whose name ends in `staff`, `bot` or `bots`.
//! Those teams exist to give the foundation and its automation the access they
//! need to administer a repository, which is not the same thing as maintaining
//! it. It is a first cut and not an answer -- somebody on `lf-staff` can also be
//! a maintainer -- so it is written once, never overwritten, and meant to be
//! edited.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::clowarden::{Mirror, team_slug};
use crate::members_model::Report;

/// The file names tried, in order.
const NAMES: [&str; 3] = ["staff.yaml", "staff.yml", "staff.json"];

/// A team whose name ends in one of these administers rather than maintains.
/// `bots` is spelled out because `bot` does not cover it.
pub const SEED_TEAM_SUFFIXES: [&str; 3] = ["staff", "bot", "bots"];

/// The name the seed is written to, when no file is there already.
pub const SEED_NAME: &str = "staff.yaml";

/// How many teams a seed line names before it stops listing them.
const TEAMS_SHOWN: usize = 3;

/// What a hand-written file can look like. Ordered: a sequence is unambiguous,
/// a mapping of lists is next, and a mapping of scalars last -- which is also
/// the only one a mapping with empty values can be.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Written {
    Listed(Vec<String>),
    Grouped(BTreeMap<String, Vec<String>>),
    WithReasons(BTreeMap<String, Option<String>>),
}

#[derive(Debug, Default)]
pub struct Staff {
    /// Lowercased login -> why, which may be empty.
    excluded: BTreeMap<String, String>,
    /// The file it came from, for saying so in the output.
    path: Option<PathBuf>,
}

impl Staff {
    /// Read whichever of the three names exists in `dir`. A file that is not
    /// there is not a problem -- most runs have no exclusions -- but one that
    /// will not parse is, because silently excluding nobody is the wrong answer
    /// to a typo.
    pub fn open(dir: &Path) -> anyhow::Result<Self> {
        let Some(path) = NAMES
            .iter()
            .map(|name| dir.join(name))
            .find(|path| path.exists())
        else {
            return Ok(Self::default());
        };
        let text = std::fs::read_to_string(&path)
            .map_err(|problem| anyhow::anyhow!("cannot read {}: {problem}", path.display()))?;
        let mut staff = Self::parse(&text).map_err(|problem| Self::unreadable(&path, problem))?;
        staff.path = Some(path);
        Ok(staff)
    }

    /// One named file, which may simply not be there.
    pub fn open_at(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)
            .map_err(|problem| anyhow::anyhow!("cannot read {}: {problem}", path.display()))?;
        let mut staff = Self::parse(&text).map_err(|problem| Self::unreadable(path, problem))?;
        staff.path = Some(path.to_path_buf());
        Ok(staff)
    }

    fn unreadable(path: &Path, problem: serde_yaml_ng::Error) -> anyhow::Error {
        anyhow::anyhow!(
            "{} is not a list of logins, a login: reason mapping, or groups of \
             logins: {problem}",
            path.display()
        )
    }

    fn parse(text: &str) -> Result<Self, serde_yaml_ng::Error> {
        // serde_yaml_ng reads JSON too, so one parser covers both spellings.
        let written: Written = serde_yaml_ng::from_str(text)?;
        let mut excluded: BTreeMap<String, String> = BTreeMap::new();
        let mut note = |login: &str, why: &str| {
            let login = login.trim();
            if login.is_empty() {
                return;
            }
            excluded.insert(login.to_lowercase(), why.trim().to_string());
        };
        match written {
            Written::Listed(logins) => {
                for login in logins {
                    note(&login, "");
                }
            }
            Written::Grouped(groups) => {
                for (group, logins) in groups {
                    for login in logins {
                        note(&login, &group);
                    }
                }
            }
            Written::WithReasons(entries) => {
                for (login, why) in entries {
                    note(&login, why.as_deref().unwrap_or_default());
                }
            }
        }
        Ok(Self {
            excluded,
            path: None,
        })
    }

    pub fn excludes(&self, login: &str) -> bool {
        self.excluded.contains_key(&login.trim().to_lowercase())
    }

    /// Why this login is excluded, when the file said. An empty string means it
    /// was listed without a reason.
    pub fn reason(&self, login: &str) -> Option<&str> {
        self.excluded
            .get(&login.trim().to_lowercase())
            .map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.excluded.len()
    }

    pub fn is_empty(&self) -> bool {
        self.excluded.is_empty()
    }

    /// The file this came from, for a one-line "read N logins from X".
    pub fn source(&self) -> Option<String> {
        self.path.as_ref().map(|path| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        })
    }

    /// Partition logins into the ones to keep and the ones the file excludes.
    /// The excluded ones come back as `login (why)` so a report can say what
    /// the file claimed, which is the only record of the judgement.
    pub fn dropped<'a>(&self, logins: impl Iterator<Item = &'a str>) -> Vec<String> {
        let mut dropped: Vec<String> = logins
            .filter(|login| self.excludes(login))
            .map(|login| match self.reason(login) {
                Some(why) if !why.is_empty() => format!("{login} ({why})"),
                _ => login.to_string(),
            })
            .collect();
        dropped.sort_by_key(|entry| entry.to_lowercase());
        dropped
    }
}

/// Does this team's name put its members in the first cut?
pub fn is_seed_team(name: &str) -> bool {
    let slug = team_slug(name);
    SEED_TEAM_SUFFIXES
        .iter()
        .any(|suffix| slug.ends_with(suffix))
}

/// Everyone on such a team, from both sides: what the configs declare, and what
/// GitHub shows. Returns login -> the teams that put them there, which becomes
/// the reason on each line.
pub fn seed(mirror: &Mirror, exports: &[(String, Report)]) -> BTreeMap<String, BTreeSet<String>> {
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut note = |login: &str, place: String| {
        let login = login.trim();
        if !login.is_empty() {
            found.entry(login.to_string()).or_default().insert(place);
        }
    };

    for ((org, team), roster) in mirror.rosters() {
        if is_seed_team(&team) {
            for login in roster {
                note(&login, format!("{org}/{team}"));
            }
        }
    }
    for (_, export) in exports {
        for person in &export.people {
            for membership in &person.organizations {
                for team in &membership.teams {
                    let name = if team.slug.is_empty() {
                        team.name.as_deref().unwrap_or_default()
                    } else {
                        &team.slug
                    };
                    if is_seed_team(name) {
                        note(
                            &person.login,
                            format!("{}/{}", membership.org, team_slug(name)),
                        );
                    }
                }
            }
        }
    }
    found
}

/// The seed as a YAML mapping of `login: why`, with a header saying where it
/// came from. One line per person, so the way to keep somebody's vote is to
/// delete their line.
pub fn render_seed(found: &BTreeMap<String, BTreeSet<String>>, today: &str) -> String {
    let mut out = String::new();
    for line in [
        "# Logins kept off both TAC election rolls, with why. Edit by hand:",
        "# delete the line for anyone who should keep their vote.",
        "#",
        &format!("# First cut written {today} by `tac-eligibility --seed-staff`:"),
        "# everyone a CLOWarden config or GitHub puts on a team whose name ends",
        "# in staff, bot or bots. Some of them are people who look after a bot",
        "# rather than automation, so read it before it counts.",
        "#",
        "# Nothing regenerates this file. It is yours now.",
    ] {
        out.push_str(line);
        out.push('\n');
    }
    if found.is_empty() {
        out.push_str("# No such team had any members.\n");
        return out;
    }
    out.push('\n');
    // Sorted case-insensitively, the way the configs themselves are kept.
    let mut lines: Vec<(&String, &BTreeSet<String>)> = found.iter().collect();
    lines.sort_by_key(|(login, _)| login.to_lowercase());
    for (login, teams) in lines {
        let shown: Vec<&str> = teams.iter().take(TEAMS_SHOWN).map(String::as_str).collect();
        let rest = teams.len().saturating_sub(shown.len());
        let why = if rest > 0 {
            format!("{} (+{rest} more)", shown.join(", "))
        } else {
            shown.join(", ")
        };
        out.push_str(&format!("{login}: {why}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn staff_from(case: &str, name: &str, text: &str) -> Staff {
        let dir = std::env::temp_dir().join(format!("tac-staff-{}-{case}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join(name), text).expect("write");
        let staff = Staff::open(&dir).expect("opens");
        std::fs::remove_dir_all(&dir).ok();
        staff
    }

    #[test]
    fn a_plain_list_of_logins_is_enough() {
        let staff = staff_from("list", "staff.yaml", "- Some-Staffer\n- some-bot\n");
        assert_eq!(staff.len(), 2);
        assert!(
            staff.excludes("some-staffer"),
            "matching is case-insensitive"
        );
        assert!(staff.excludes("SOME-BOT"));
        assert!(!staff.excludes("a-maintainer"));
        assert_eq!(staff.reason("some-bot"), Some(""));
        assert_eq!(staff.source().as_deref(), Some("staff.yaml"));
    }

    #[test]
    fn a_mapping_keeps_the_reason_and_tolerates_an_empty_one() {
        let staff = staff_from(
            "reasons",
            "staff.yaml",
            "some-staffer: foundation staff\nsome-bot:\n",
        );
        assert_eq!(staff.reason("some-staffer"), Some("foundation staff"));
        assert_eq!(staff.reason("some-bot"), Some(""));
    }

    #[test]
    fn groups_become_the_reason() {
        let staff = staff_from(
            "groups",
            "staff.yml",
            "staff:\n  - some-staffer\nbots:\n  - some-bot\n",
        );
        assert_eq!(staff.reason("some-staffer"), Some("staff"));
        assert_eq!(staff.reason("some-bot"), Some("bots"));
        assert_eq!(staff.source().as_deref(), Some("staff.yml"));
    }

    #[test]
    fn json_is_read_by_the_same_parser() {
        let staff = staff_from(
            "json",
            "staff.json",
            "{\"some-staffer\": \"foundation staff\"}",
        );
        assert_eq!(staff.reason("some-staffer"), Some("foundation staff"));
        assert_eq!(staff.source().as_deref(), Some("staff.json"));
    }

    #[test]
    fn no_file_excludes_nobody() {
        let dir = std::env::temp_dir().join(format!("tac-staff-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let staff = Staff::open(&dir).expect("opens");
        std::fs::remove_dir_all(&dir).ok();
        assert!(staff.is_empty());
        assert!(staff.source().is_none());
        assert!(!staff.excludes("anyone"));
    }

    #[test]
    fn a_file_that_will_not_parse_stops_the_run() {
        let dir = std::env::temp_dir().join(format!("tac-staff-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("staff.yaml"), "- [a, b]\n- {c: d}\n").expect("write");
        let problem = Staff::open(&dir).expect_err("rejects");
        std::fs::remove_dir_all(&dir).ok();
        assert!(
            problem.to_string().contains("is not a list of logins"),
            "{problem}"
        );
    }

    #[test]
    fn a_team_name_ending_in_staff_or_bot_seeds_the_file() {
        for yes in [
            "lf-staff",
            "staff",
            "LF Staff",
            "robots",
            "hyperledger-bot",
            "Bots",
        ] {
            assert!(is_seed_team(yes), "{yes} should seed");
        }
        for no in [
            "maintainers",
            "staffing-committee",
            "bottle-makers",
            "committers",
        ] {
            assert!(!is_seed_team(no), "{no} should not seed");
        }
    }

    #[test]
    fn the_seed_reads_both_the_configs_and_the_live_teams() {
        use crate::clowarden::{Config, Repository, Team};

        let mirror = Mirror {
            configs: vec![Config {
                org: "acme".into(),
                path: "config.yaml".into(),
                teams: vec![
                    Team {
                        name: "lf-staff".into(),
                        slug: "lf-staff".into(),
                        maintainers: vec!["a-staffer".into()],
                        members: vec!["another-staffer".into()],
                    },
                    Team {
                        name: "Maintainers".into(),
                        slug: "maintainers".into(),
                        maintainers: vec!["a-maintainer".into()],
                        members: vec![],
                    },
                ],
                repositories: vec![Repository {
                    name: "widget".into(),
                    teams: Default::default(),
                    collaborators: Default::default(),
                }],
            }],
            skipped: vec![],
        };
        let export: Report = serde_json::from_value(serde_json::json!({
            "source": {"api_url": "https://api.github.com", "include_child_team_members": true},
            "organizations": ["acme"],
            "totals": {"organizations": 1, "people": 0, "teams": 0},
            "people": [
                {"login": "live-only-staffer", "organizations": [
                    {"org": "acme", "role": "MEMBER",
                     "teams": [{"slug": "lf-staff", "role": "MEMBER"}]}]},
                {"login": "a-maintainer", "organizations": [
                    {"org": "acme", "role": "MEMBER",
                     "teams": [{"slug": "maintainers", "role": "MEMBER"}]}]},
            ],
        }))
        .expect("fixture");

        let found = seed(&mirror, &[("peeps/acme.yaml".to_string(), export)]);
        let logins: Vec<&String> = found.keys().collect();
        assert_eq!(
            logins,
            ["a-staffer", "another-staffer", "live-only-staffer"],
            "a maintainer is not staff; somebody GitHub shows on the team is"
        );
        assert_eq!(
            found["live-only-staffer"].iter().collect::<Vec<_>>(),
            ["acme/lf-staff"]
        );

        let rendered = render_seed(&found, "2026-10-10");
        assert!(rendered.starts_with("# Logins kept off both TAC election rolls"));
        assert!(rendered.contains("a-staffer: acme/lf-staff\n"));
        // And it reads back as the mapping shape, reasons and all.
        let staff = Staff::parse(&rendered).expect("round-trips");
        assert!(staff.excludes("A-Staffer"));
        assert_eq!(staff.reason("live-only-staffer"), Some("acme/lf-staff"));
        assert!(!staff.excludes("a-maintainer"));
    }

    #[test]
    fn dropped_names_what_was_excluded_and_why() {
        let staff = staff_from("dropped", "staff.yaml", "zoe-staffer: staff\nsome-bot:\n");
        let dropped = staff.dropped(["a-maintainer", "Some-Bot", "zoe-staffer"].into_iter());
        assert_eq!(dropped, ["Some-Bot", "zoe-staffer (staff)"]);
    }
}
