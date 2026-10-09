//! Who may vote: the maintainers.
//!
//! "Maintainer" is not a GitHub role, so it has to be derived. Someone counts
//! if a CLOWarden config puts them on a team that holds `write`, `maintain` or
//! `admin` on some repository, or if they own an organization. Three sources
//! agree about most people and disagree about enough of them to be worth
//! recording per person:
//!
//! - `config` -- the config's own roster for such a team.
//! - `live` -- the team's actual membership, from a `gh-org-members` export,
//!   which can include people the config never named.
//! - `owner` -- an organization owner, who can reach every repository in it
//!   whatever the teams say.
//!
//! No network: both sides are files on disk.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::clowarden::{Mirror, team_slug};
use crate::members_model::Report;
use crate::tac::is_bot;

/// One voter, and why.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Voter {
    /// The login as the source spells it.
    pub login: String,
    /// `config`, `live`, `owner` -- which of the three put them on the list.
    pub sources: Vec<String>,
    /// `org/team` for each place that did, so a surprise can be traced back.
    /// Spelled `where_` because `where` is a Rust keyword; the file says
    /// `where`.
    #[serde(rename = "where")]
    pub where_: Vec<String>,
}

impl Voter {
    fn new(login: &str) -> Self {
        Voter {
            login: login.to_string(),
            sources: Vec::new(),
            where_: Vec::new(),
        }
    }
}

/// `voters.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Voters {
    /// Organizations with a readable config.
    pub clowarden_orgs: Vec<String>,
    /// Organizations in the mirror with no config, or none that would parse:
    /// nothing is declared about their access.
    pub orgs_without_config: Vec<String>,
    pub people_export: String,
    /// How many teams hold write or better somewhere.
    pub write_teams: usize,
    pub bots_dropped: Vec<String>,
    /// Keyed by lowercased login, which is how GitHub compares them.
    pub voters: BTreeMap<String, Voter>,
}

/// Build the voter list from the configs and the export.
pub fn compute(
    mirror: &Mirror,
    export: &Report,
    people_export: &str,
) -> (Voters, Vec<(String, usize)>) {
    let write_teams = mirror.write_teams();
    let mut voters: BTreeMap<String, Voter> = BTreeMap::new();

    let note = |voters: &mut BTreeMap<String, Voter>, login: &str, source: &str, place: String| {
        let entry = voters
            .entry(login.to_lowercase())
            .or_insert_with(|| Voter::new(login));
        if !entry.sources.iter().any(|s| s == source) {
            entry.sources.push(source.to_string());
        }
        if !entry.where_.contains(&place) {
            entry.where_.push(place);
        }
    };

    // What the configs declare.
    for ((org, team), roster) in mirror.rosters() {
        if write_teams.contains(&(org.clone(), team.clone())) {
            for login in roster {
                note(&mut voters, &login, "config", format!("{org}/{team}"));
            }
        }
    }

    // What GitHub shows: live membership of those same teams, and ownership.
    for person in &export.people {
        let login = &person.login;
        if login.is_empty() {
            continue;
        }
        for membership in &person.organizations {
            let org = &membership.org;
            if membership
                .role
                .as_deref()
                .is_some_and(|role| role.eq_ignore_ascii_case("ADMIN"))
            {
                note(&mut voters, login, "owner", format!("{org}/owners"));
            }
            for team in &membership.teams {
                let slug = if team.slug.is_empty() {
                    team_slug(team.name.as_deref().unwrap_or_default())
                } else {
                    team_slug(&team.slug)
                };
                if write_teams.contains(&(org.clone(), slug)) {
                    note(&mut voters, login, "live", format!("{org}/{}", team.slug));
                }
            }
        }
    }

    let bots: Vec<String> = voters
        .keys()
        .filter(|login| is_bot(login))
        .cloned()
        .collect();
    for bot in &bots {
        voters.remove(bot);
    }

    // How many people each combination of sources accounts for -- the quickest
    // read on whether the config and GitHub are telling the same story.
    let mut by_source: BTreeMap<String, usize> = BTreeMap::new();
    for voter in voters.values_mut() {
        voter.sources.sort();
        voter.where_.sort();
        *by_source.entry(voter.sources.join("+")).or_default() += 1;
    }
    let mut combinations: Vec<(String, usize)> = by_source.into_iter().collect();
    combinations.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let orgs_without_config: BTreeSet<String> = mirror.skipped.iter().cloned().collect();
    (
        Voters {
            clowarden_orgs: {
                let mut orgs = mirror.orgs();
                orgs.sort();
                orgs
            },
            orgs_without_config: orgs_without_config.into_iter().collect(),
            people_export: people_export.to_string(),
            write_teams: write_teams.len(),
            bots_dropped: bots,
            voters,
        },
        combinations,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clowarden::{Config, Repository, Team};

    fn mirror() -> Mirror {
        Mirror {
            configs: vec![Config {
                org: "acme".into(),
                path: "config.yaml".into(),
                teams: vec![
                    Team {
                        name: "Maintainers".into(),
                        slug: "maintainers".into(),
                        maintainers: vec!["alice".into()],
                        members: vec!["bob".into()],
                    },
                    Team {
                        name: "Readers".into(),
                        slug: "readers".into(),
                        maintainers: vec![],
                        members: vec!["carol".into()],
                    },
                ],
                repositories: vec![Repository {
                    name: "widget".into(),
                    teams: [("Maintainers".to_string(), "maintain".to_string())]
                        .into_iter()
                        .collect(),
                    collaborators: Default::default(),
                }],
            }],
            skipped: vec!["lonely-org".into()],
        }
    }

    fn export(people: serde_json::Value) -> Report {
        serde_json::from_value(serde_json::json!({
            "source": {"api_url": "https://api.github.com", "include_child_team_members": true},
            "organizations": ["acme"],
            "totals": {"organizations": 1, "people": 0, "teams": 0},
            "people": people,
        }))
        .expect("fixture deserializes")
    }

    #[test]
    fn a_config_roster_on_a_write_team_votes_and_a_read_team_does_not() {
        let (voters, _) = compute(&mirror(), &export(serde_json::json!([])), "people.yaml");
        let logins: Vec<&String> = voters.voters.keys().collect();
        assert_eq!(logins, ["alice", "bob"], "carol only has read");
        assert_eq!(voters.voters["alice"].sources, ["config"]);
        assert_eq!(voters.voters["alice"].where_, ["acme/maintainers"]);
        assert_eq!(voters.write_teams, 1);
        assert_eq!(voters.orgs_without_config, ["lonely-org"]);
    }

    #[test]
    fn live_membership_and_ownership_add_people_the_config_never_named() {
        let (voters, combinations) = compute(
            &mirror(),
            &export(serde_json::json!([
                {"login": "alice", "organizations": [
                    {"org": "acme", "role": "MEMBER",
                     "teams": [{"slug": "maintainers", "role": "MAINTAINER"}]}]},
                {"login": "dave", "organizations": [
                    {"org": "acme", "role": "MEMBER",
                     "teams": [{"slug": "maintainers", "role": "MEMBER"}]}]},
                {"login": "erin", "organizations": [
                    {"org": "acme", "role": "ADMIN", "teams": []}]},
                {"login": "frank", "organizations": [
                    {"org": "acme", "role": "MEMBER",
                     "teams": [{"slug": "readers", "role": "MEMBER"}]}]},
            ])),
            "people.yaml",
        );
        assert_eq!(voters.voters["alice"].sources, ["config", "live"]);
        assert_eq!(voters.voters["dave"].sources, ["live"], "not in the config");
        assert_eq!(voters.voters["erin"].sources, ["owner"]);
        assert_eq!(voters.voters["erin"].where_, ["acme/owners"]);
        assert!(!voters.voters.contains_key("frank"), "readers do not vote");
        assert_eq!(
            combinations,
            [
                ("config".to_string(), 1), // bob
                ("config+live".to_string(), 1),
                ("live".to_string(), 1),
                ("owner".to_string(), 1),
            ],
            "ties ordered by combination so a run is reproducible"
        );
    }

    #[test]
    fn a_bot_on_a_write_team_is_dropped_and_said_so() {
        let mut mirror = mirror();
        mirror.configs[0].teams[0].members.push("lfdt-bot".into());
        let (voters, _) = compute(&mirror, &export(serde_json::json!([])), "people.yaml");
        assert_eq!(voters.bots_dropped, ["lfdt-bot"]);
        assert!(!voters.voters.contains_key("lfdt-bot"));
    }

    #[test]
    fn one_person_keeps_the_spelling_the_source_used() {
        let mut mirror = mirror();
        mirror.configs[0].teams[0].maintainers = vec!["RyJones".into()];
        let (voters, _) = compute(&mirror, &export(serde_json::json!([])), "people.yaml");
        assert_eq!(voters.voters["ryjones"].login, "RyJones");
    }
}
