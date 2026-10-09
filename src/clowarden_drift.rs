//! Reconciling a CLOWarden config against a `gh-org-members` export.
//!
//! The config is a statement of intent; the export is the state. Where the two
//! disagree, either someone holds access nobody declared, or a declaration
//! never took effect. Each disagreement becomes one record, so the ones worth
//! looking into are named rather than counted.
//!
//! Only organizations that have a config are compared. One without a config has
//! declared nothing, which is not the same as declaring that nobody has access.

use std::collections::BTreeMap;

use serde::{Serialize, Serializer};
use serde::ser::SerializeMap;

use crate::clowarden::{team_slug, Mirror};
use crate::members_model::Report;

/// Which way a team's membership disagrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The config does not mention this team at all, yet it exists and has
    /// members.
    TeamNotInConfig,
    /// The team is declared, but this person is not on it.
    OnGithubNotInConfig,
    /// Both sides have the person, with different roles.
    RoleMismatch,
    /// The config declares a membership GitHub does not have.
    InConfigNotOnGithub,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::TeamNotInConfig => "team_not_in_config",
            Kind::OnGithubNotInConfig => "on_github_not_in_config",
            Kind::RoleMismatch => "role_mismatch",
            Kind::InConfigNotOnGithub => "in_config_not_on_github",
        }
    }
}

/// One disagreement about one person on one team.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub login: String,
    pub org: String,
    pub team: String,
    pub kind: Kind,
    /// The role GitHub shows, or empty when GitHub does not have the person.
    pub github_role: String,
    /// The role the config declares, or empty when the config does not.
    pub config_role: String,
}

/// The columns of `drift.csv`, in order.
pub const FIELDS: [&str; 6] = [
    "login",
    "org",
    "team",
    "kind",
    "github_role",
    "config_role",
];

/// `(org_lower, team_slug) -> login_lower -> (role, login as written)`.
///
/// Logins are keyed lowercased because GitHub treats them case-insensitively
/// and configs are hand-written, but the spelling a human chose is kept for the
/// report.
type TeamState = BTreeMap<(String, String), BTreeMap<String, (String, String)>>;

/// What the configs declare.
pub fn declared(mirror: &Mirror) -> TeamState {
    let mut state: TeamState = BTreeMap::new();
    for config in &mirror.configs {
        for team in &config.teams {
            let key = (config.org.to_lowercase(), team.slug.clone());
            let members = state.entry(key).or_default();
            for (role, logins) in [("MAINTAINER", &team.maintainers), ("MEMBER", &team.members)] {
                for login in logins {
                    let login = login.trim();
                    if !login.is_empty() {
                        members.insert(
                            login.to_lowercase(),
                            (role.to_string(), login.to_string()),
                        );
                    }
                }
            }
        }
    }
    state
}

/// What the export shows. A team role GitHub did not report reads as `MEMBER`,
/// which is what GitHub itself defaults it to.
pub fn actual(export: &Report) -> TeamState {
    let mut state: TeamState = BTreeMap::new();
    for person in &export.people {
        for org in &person.organizations {
            for team in &org.teams {
                let key = (org.org.to_lowercase(), team_slug(&team.slug));
                state.entry(key).or_default().insert(
                    person.login.to_lowercase(),
                    (
                        team.role.clone().unwrap_or_else(|| "MEMBER".to_string()),
                        person.login.clone(),
                    ),
                );
            }
        }
    }
    state
}

/// Every disagreement, ordered by organization, then team, then login.
pub fn reconcile(declared: &TeamState, actual: &TeamState, managed_orgs: &[String]) -> Vec<Finding> {
    let managed: std::collections::BTreeSet<String> =
        managed_orgs.iter().map(|org| org.to_lowercase()).collect();

    let mut keys: Vec<&(String, String)> = declared.keys().chain(actual.keys()).collect();
    keys.sort();
    keys.dedup();

    let mut findings = Vec::new();
    for key in keys {
        let (org, team) = key;
        if !managed.contains(org) {
            continue; // no config to disagree with
        }
        let in_config = declared.contains_key(key);
        let config_side = declared.get(key);
        let github_side = actual.get(key);

        for (login_lower, (role, login)) in github_side.into_iter().flatten() {
            let declared_role = config_side.and_then(|side| side.get(login_lower));
            let kind = if !in_config {
                Kind::TeamNotInConfig
            } else if declared_role.is_none() {
                Kind::OnGithubNotInConfig
            } else if declared_role.map(|(role, _)| role.as_str()) != Some(role.as_str()) {
                Kind::RoleMismatch
            } else {
                continue;
            };
            findings.push(Finding {
                login: login.clone(),
                org: org.clone(),
                team: team.clone(),
                kind,
                github_role: role.clone(),
                config_role: declared_role
                    .map(|(role, _)| role.clone())
                    .unwrap_or_default(),
            });
        }

        for (login_lower, (role, login)) in config_side.into_iter().flatten() {
            if github_side.is_none_or(|side| !side.contains_key(login_lower)) {
                findings.push(Finding {
                    login: login.clone(),
                    org: org.clone(),
                    team: team.clone(),
                    kind: Kind::InConfigNotOnGithub,
                    github_role: String::new(),
                    config_role: role.clone(),
                });
            }
        }
    }

    findings.sort_by(|a, b| {
        (a.org.to_lowercase(), &a.team, a.login.to_lowercase())
            .cmp(&(b.org.to_lowercase(), &b.team, b.login.to_lowercase()))
    });
    findings
}

/// Counts in the order they are reported — biggest first, not alphabetical —
/// written as a JSON object all the same.
#[derive(Debug, Default, Clone)]
pub struct Counts(pub Vec<(String, usize)>);

impl Counts {
    /// Tally `keys`, most common first, ties broken by key so a run is
    /// reproducible.
    pub fn most_common(keys: impl Iterator<Item = String>) -> Self {
        let mut tally: BTreeMap<String, usize> = BTreeMap::new();
        for key in keys {
            *tally.entry(key).or_default() += 1;
        }
        let mut pairs: Vec<(String, usize)> = tally.into_iter().collect();
        pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Counts(pairs)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The same counts, alphabetically, for the totals block.
    pub fn sorted(&self) -> Vec<(String, usize)> {
        let mut pairs = self.0.clone();
        pairs.sort();
        pairs
    }
}

impl Serialize for Counts {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, count) in &self.0 {
            map.serialize_entry(key, count)?;
        }
        map.end()
    }
}

/// `drift.json`. The field order is the reading order: where the two sides came
/// from, then the totals, then who and where, then the findings themselves.
#[derive(Debug, Serialize)]
pub struct DriftReport {
    pub source: Source,
    pub totals: Totals,
    pub by_org: Counts,
    pub by_login: Counts,
    pub findings: Vec<Finding>,
}

#[derive(Debug, Serialize)]
pub struct Source {
    pub mirror: String,
    pub people: String,
    pub authenticated_as: Option<String>,
    pub organizations_with_config: usize,
    /// Organizations in the export that no config manages.
    pub organizations_without_config: Vec<String>,
    pub method: String,
}

#[derive(Debug, Serialize)]
pub struct Totals {
    pub findings: usize,
    pub logins: usize,
    /// One count per kind of disagreement, alphabetically.
    #[serde(flatten)]
    pub kinds: Counts,
}

/// `drift.csv`: every field quoted, CRLF line endings, as the csv module writes
/// it with `QUOTE_NONNUMERIC`.
pub fn csv(findings: &[Finding]) -> String {
    let mut out = String::new();
    push_row(&mut out, FIELDS.iter().copied());
    for finding in findings {
        push_row(
            &mut out,
            [
                finding.login.as_str(),
                finding.org.as_str(),
                finding.team.as_str(),
                finding.kind.as_str(),
                finding.github_role.as_str(),
                finding.config_role.as_str(),
            ]
            .into_iter(),
        );
    }
    out
}

fn push_row<'a>(out: &mut String, fields: impl Iterator<Item = &'a str>) {
    let quoted: Vec<String> = fields
        .map(|field| format!("\"{}\"", field.replace('"', "\"\"")))
        .collect();
    out.push_str(&quoted.join(","));
    out.push_str("\r\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clowarden::{Config, Team};

    fn mirror_with(org: &str, team: &str, maintainers: &[&str], members: &[&str]) -> Mirror {
        Mirror {
            configs: vec![Config {
                org: org.to_string(),
                path: "config.yaml".into(),
                teams: vec![Team {
                    name: team.to_string(),
                    slug: team_slug(team),
                    maintainers: maintainers.iter().map(|s| s.to_string()).collect(),
                    members: members.iter().map(|s| s.to_string()).collect(),
                }],
                repositories: Vec::new(),
            }],
            skipped: Vec::new(),
        }
    }

    fn export_with(rows: &[(&str, &str, &str, &str)]) -> Report {
        let people: Vec<serde_json::Value> = rows
            .iter()
            .map(|(login, org, team, role)| {
                serde_json::json!({
                    "login": login,
                    "organizations": [{
                        "org": org,
                        "role": "MEMBER",
                        "teams": [{"slug": team, "role": role}],
                    }],
                })
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "source": {"api_url": "https://api.github.com", "include_child_team_members": true},
            "organizations": ["acme"],
            "totals": {"organizations": 1, "people": 0, "teams": 0},
            "people": people,
        }))
        .expect("fixture deserializes")
    }

    #[test]
    fn a_membership_both_sides_agree_on_is_not_a_finding() {
        let mirror = mirror_with("acme", "Maintainers", &["alice"], &[]);
        let export = export_with(&[("alice", "acme", "maintainers", "MAINTAINER")]);
        let findings = reconcile(&declared(&mirror), &actual(&export), &mirror.orgs());
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn each_way_of_disagreeing_is_its_own_kind() {
        let mirror = mirror_with("acme", "Maintainers", &["alice"], &["bob", "ghost"]);
        let export = export_with(&[
            ("alice", "acme", "maintainers", "MAINTAINER"), // agrees
            ("bob", "acme", "maintainers", "MAINTAINER"),   // config says MEMBER
            ("carol", "acme", "maintainers", "MEMBER"),     // not in the config
            ("dave", "acme", "secret-team", "MEMBER"),      // team not in the config
        ]);
        let findings = reconcile(&declared(&mirror), &actual(&export), &mirror.orgs());
        let kinds: Vec<(&str, Kind)> = findings
            .iter()
            .map(|f| (f.login.as_str(), f.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("bob", Kind::RoleMismatch),
                ("carol", Kind::OnGithubNotInConfig),
                ("ghost", Kind::InConfigNotOnGithub),
                ("dave", Kind::TeamNotInConfig),
            ],
            "ordered by org, then team, then login: maintainers before secret-team"
        );
    }

    #[test]
    fn an_organization_with_no_config_is_left_alone() {
        let mirror = mirror_with("acme", "Maintainers", &["alice"], &[]);
        let export = export_with(&[
            ("alice", "acme", "maintainers", "MAINTAINER"),
            ("someone", "other-org", "whoever", "MEMBER"),
        ]);
        let findings = reconcile(&declared(&mirror), &actual(&export), &mirror.orgs());
        assert!(
            findings.is_empty(),
            "other-org declared nothing, so it can disagree with nothing: {findings:?}"
        );
    }

    #[test]
    fn a_login_differing_only_in_case_is_the_same_person() {
        let mirror = mirror_with("acme", "Maintainers", &["Alice"], &[]);
        let export = export_with(&[("alice", "acme", "maintainers", "MAINTAINER")]);
        let findings = reconcile(&declared(&mirror), &actual(&export), &mirror.orgs());
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn counts_run_biggest_first_and_json_keeps_that_order() {
        let counts = Counts::most_common(
            ["b", "a", "b", "c", "b", "a"]
                .into_iter()
                .map(str::to_string),
        );
        assert_eq!(
            counts.0,
            [("b".into(), 3), ("a".into(), 2), ("c".into(), 1)]
        );
        assert_eq!(
            serde_json::to_string(&counts).expect("serializes"),
            r#"{"b":3,"a":2,"c":1}"#
        );
    }

    #[test]
    fn the_csv_quotes_every_field_and_doubles_a_quote() {
        let findings = vec![Finding {
            login: "al\"ice".into(),
            org: "acme".into(),
            team: "maintainers".into(),
            kind: Kind::RoleMismatch,
            github_role: "MEMBER".into(),
            config_role: "MAINTAINER".into(),
        }];
        let text = csv(&findings);
        let mut lines = text.split("\r\n");
        assert_eq!(
            lines.next().unwrap(),
            r#""login","org","team","kind","github_role","config_role""#
        );
        assert_eq!(
            lines.next().unwrap(),
            r#""al""ice","acme","maintainers","role_mismatch","MEMBER","MAINTAINER""#
        );
    }
}
