//! What changed between two election runs, and why.
//!
//! The phase files say who is on each roll *now*. A publish wants the other
//! question — who joined, who left, and for what reason — because a roll that
//! moves without an explanation is a roll nobody can check. Each file carries
//! enough to answer it: `voters.json` lists the maintainers it dropped for
//! having no activity (`cannot_run_dropped`), the staff file's exclusions and
//! the bots, so a login that has left the roll can be found in whichever list
//! now holds it, and a login that has joined can be found in the list that
//! held it before.
//!
//! The four reasons, in the order they are checked:
//!
//! | a voter left because | the new file shows them |
//! | --- | --- |
//! | no activity | in `cannot_run_dropped` |
//! | added to the staff list | in `staff_dropped` |
//! | counted as a bot | in `bots_dropped` |
//! | no longer a maintainer | nowhere at all |
//!
//! and the reverse for a voter who joined, against the old file. The nominee
//! roll has no maintainer half, so a nominee joins or leaves on activity
//! alone, staff and bots aside.
//!
//! No network, no git: two parsed files in, a commit message out.

use std::collections::{BTreeMap, BTreeSet};

use crate::tac::Window;
use crate::tac_logins::Logins;
use crate::tac_voters::Voters;

/// One roll as a run left it: who is on it, and the lists that explain who is
/// not.
#[derive(Debug, Default, Clone)]
pub struct Roll {
    /// Lowercased login -> the spelling the file used.
    pub on: BTreeMap<String, String>,
    /// Maintainers kept off for having no activity in the window. Always empty
    /// for the nominee roll, which *is* the activity.
    pub no_activity: BTreeSet<String>,
    pub staff: BTreeSet<String>,
    pub bots: BTreeSet<String>,
}

impl Roll {
    pub fn of_voters(voters: &Voters) -> Roll {
        Roll {
            on: voters
                .voters
                .iter()
                .map(|(key, voter)| (key.clone(), voter.login.clone()))
                .collect(),
            no_activity: keys(&voters.cannot_run_dropped),
            staff: keys(&voters.staff_dropped),
            bots: keys(&voters.bots_dropped),
        }
    }

    pub fn of_nominees(nominees: &Logins) -> Roll {
        Roll {
            on: nominees
                .logins
                .iter()
                .map(|(key, account)| (key.clone(), account.login.clone()))
                .collect(),
            no_activity: BTreeSet::new(),
            staff: keys(&nominees.staff_dropped),
            bots: keys(&nominees.bots_dropped),
        }
    }
}

/// The dropped lists are written for people to read: the staff one spells its
/// entries `login (reason)`. Compare on the login alone, lowercased, the way
/// GitHub does.
fn keys(entries: &[String]) -> BTreeSet<String> {
    entries.iter().map(|entry| login_of(entry)).collect()
}

fn login_of(entry: &str) -> String {
    entry
        .split_once(" (")
        .map(|(login, _)| login)
        .unwrap_or(entry)
        .trim()
        .to_lowercase()
}

/// Both rolls, as one run left them.
#[derive(Debug, Default, Clone)]
pub struct Snapshot {
    pub voters: Roll,
    pub nominees: Roll,
}

/// Which roll is being described: the reasons are worded for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Which {
    Voters,
    Nominees,
}

/// One reason, and everyone it accounts for, in the order they should be read:
/// the ordinary reason first, then the unusual ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub reason: &'static str,
    pub logins: Vec<String>,
}

/// Who joined and who left one roll, each grouped by why.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    pub joined: Vec<Group>,
    pub left: Vec<Group>,
    pub joined_count: usize,
    pub left_count: usize,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.joined_count == 0 && self.left_count == 0
    }
}

fn changes(before: &Roll, after: &Roll, which: Which) -> Changes {
    // Reason -> logins, in the spelling of the file that has them, ordered by
    // the lowercased login so a rerun says the same thing.
    let mut joined: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    let mut left: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();

    for (key, login) in &after.on {
        if before.on.contains_key(key) {
            continue;
        }
        let reason = if before.no_activity.contains(key) {
            "now active in the window"
        } else if before.staff.contains(key) {
            "off the staff list"
        } else if before.bots.contains(key) {
            "no longer counted as a bot"
        } else if which == Which::Voters {
            "added maintainer"
        } else {
            "activity in the window"
        };
        joined.entry(reason).or_default().push(login.clone());
    }

    for (key, login) in &before.on {
        if after.on.contains_key(key) {
            continue;
        }
        let reason = if after.no_activity.contains(key) {
            "no activity"
        } else if after.staff.contains(key) {
            "added to the staff list"
        } else if after.bots.contains(key) {
            "counted as a bot"
        } else if which == Which::Voters {
            "no longer a maintainer"
        } else {
            "no activity in the window"
        };
        left.entry(reason).or_default().push(login.clone());
    }

    Changes {
        joined_count: joined.values().map(Vec::len).sum(),
        left_count: left.values().map(Vec::len).sum(),
        joined: ordered(joined, JOINED_ORDER),
        left: ordered(left, LEFT_ORDER),
    }
}

/// The ordinary reason first: that is the one a reader is checking for.
const JOINED_ORDER: [&str; 5] = [
    "added maintainer",
    "activity in the window",
    "now active in the window",
    "off the staff list",
    "no longer counted as a bot",
];
const LEFT_ORDER: [&str; 5] = [
    "no activity",
    "no activity in the window",
    "no longer a maintainer",
    "added to the staff list",
    "counted as a bot",
];

fn ordered(groups: BTreeMap<&'static str, Vec<String>>, order: [&str; 5]) -> Vec<Group> {
    let mut groups: Vec<Group> = groups
        .into_iter()
        .map(|(reason, logins)| Group { reason, logins })
        .collect();
    groups.sort_by_key(|group| {
        order
            .iter()
            .position(|reason| *reason == group.reason)
            .unwrap_or(order.len())
    });
    groups
}

/// The commit message for a publish: a subject that carries the numbers, then
/// a paragraph per roll naming everyone who moved and why.
///
/// `before` is `None` for the first publish, when there is nothing to compare
/// against and the message says so rather than claiming everyone just joined.
pub fn message(
    before: Option<&Snapshot>,
    after: &Snapshot,
    window: &Window,
    source: Option<&str>,
) -> String {
    let voters = after.voters.on.len();
    let nominees = after.nominees.on.len();
    let moved = before.map(|before| {
        (
            changes(&before.voters, &after.voters, Which::Voters),
            changes(&before.nominees, &after.nominees, Which::Nominees),
        )
    });

    let mut out = String::new();
    match &moved {
        None => out.push_str(&format!(
            "Election rolls: {voters} voters, {nominees} nominees\n"
        )),
        Some((voter_changes, nominee_changes))
            if voter_changes.is_empty() && nominee_changes.is_empty() =>
        {
            out.push_str(&format!(
                "Election data: {voters} voters, {nominees} nominees, rolls unchanged\n"
            ))
        }
        Some((voter_changes, nominee_changes)) => out.push_str(&format!(
            "Election rolls: {voters} voters ({}), {nominees} nominees ({})\n",
            delta(voter_changes),
            delta(nominee_changes)
        )),
    }

    out.push_str(&format!(
        "\nWindow {} to {}{}.\n",
        window.since,
        window.until,
        match source {
            Some(source) => format!(", from {source}"),
            None => String::new(),
        }
    ));

    match &moved {
        None => out.push_str(
            "\nFirst publish: there is no previous roll here to compare \
             against, so nothing\nis reported as joining or leaving.\n",
        ),
        Some((voter_changes, nominee_changes)) => {
            for (title, count, before_count, changes) in [
                (
                    "Voters",
                    voters,
                    before.map(|b| b.voters.on.len()).unwrap_or(0),
                    voter_changes,
                ),
                (
                    "Nominees",
                    nominees,
                    before.map(|b| b.nominees.on.len()).unwrap_or(0),
                    nominee_changes,
                ),
            ] {
                out.push('\n');
                if changes.is_empty() {
                    out.push_str(&format!("{title}: {count}, unchanged\n"));
                    continue;
                }
                out.push_str(&format!(
                    "{title}: {count}, was {before_count} ({})\n",
                    delta(changes)
                ));
                for (verb, groups) in [("joined", &changes.joined), ("left", &changes.left)] {
                    for group in groups {
                        out.push_str(&format!(
                            "  {verb} -- {} ({})\n",
                            group.reason,
                            group.logins.len()
                        ));
                        out.push_str(&wrapped(&group.logins, 4, 72));
                    }
                }
            }
        }
    }
    out
}

fn delta(changes: &Changes) -> String {
    format!("+{} / -{}", changes.joined_count, changes.left_count)
}

/// `logins`, comma-separated, wrapped to `width` columns at `indent` spaces.
///
/// A roll of five hundred people is a message nobody reads, so a very long
/// group is cut off rather than wrapped forever -- the files in the same commit
/// hold the whole list either way.
fn wrapped(logins: &[String], indent: usize, width: usize) -> String {
    const MOST: usize = 200;
    let shown = logins.len().min(MOST);
    let pad = " ".repeat(indent);
    let mut out = String::new();
    let mut line = pad.clone();

    for (index, login) in logins[..shown].iter().enumerate() {
        let last = index + 1 == shown;
        let piece = if last {
            login.clone()
        } else {
            format!("{login},")
        };
        if line.len() > indent && line.len() + 1 + piece.len() > width {
            out.push_str(line.trim_end());
            out.push('\n');
            line = pad.clone();
        }
        if line.len() > indent {
            line.push(' ');
        }
        line.push_str(&piece);
    }
    if line.len() > indent {
        out.push_str(line.trim_end());
        out.push('\n');
    }
    if logins.len() > shown {
        out.push_str(&format!("{pad}... and {} more\n", logins.len() - shown));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tac_logins::Account;
    use crate::tac_voters::Voter;

    fn window() -> Window {
        Window {
            since: "2025-10-01".into(),
            until: "now".into(),
        }
    }

    fn roll(on: &[&str]) -> Roll {
        Roll {
            on: on
                .iter()
                .map(|login| (login.to_lowercase(), login.to_string()))
                .collect(),
            ..Roll::default()
        }
    }

    fn voters(on: &[&str], no_activity: &[&str], staff: &[&str]) -> Voters {
        Voters {
            clowarden_orgs: vec![],
            orgs_without_config: vec![],
            people_exports: vec![],
            orgs_without_people: vec![],
            write_teams: 0,
            repositories: Default::default(),
            bots_dropped: vec![],
            staff_dropped: staff.iter().map(|s| s.to_string()).collect(),
            cannot_run_dropped: no_activity.iter().map(|s| s.to_string()).collect(),
            voters: on
                .iter()
                .map(|login| {
                    (
                        login.to_lowercase(),
                        Voter {
                            login: login.to_string(),
                            sources: vec![],
                            where_: vec![],
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn a_voter_roll_is_read_out_of_the_file_with_its_dropped_lists() {
        let roll = Roll::of_voters(&voters(
            &["Alice", "bob"],
            &["carol"],
            &["dave (staff)", "erin"],
        ));
        assert_eq!(roll.on["alice"], "Alice", "the file's own spelling");
        assert_eq!(roll.no_activity, ["carol".to_string()].into());
        assert_eq!(
            roll.staff,
            ["dave".to_string(), "erin".to_string()].into(),
            "the reason in parentheses is not part of the login"
        );
    }

    #[test]
    fn a_nominee_roll_comes_from_the_logins_file() {
        let nominees = Logins {
            window: window(),
            logins: [(
                "ryjones".to_string(),
                Account {
                    login: "RyJones".into(),
                    commits: 3,
                    emails: vec![],
                    repos: vec![],
                },
            )]
            .into(),
            bots_dropped: vec!["a-bot".into()],
            staff_dropped: vec![],
            unresolved: vec![],
        };
        let roll = Roll::of_nominees(&nominees);
        assert_eq!(roll.on["ryjones"], "RyJones");
        assert_eq!(roll.bots, ["a-bot".to_string()].into());
        assert!(
            roll.no_activity.is_empty(),
            "a nominee roll is the activity"
        );
    }

    /// The four reasons a voter can leave, and the four they can join by.
    #[test]
    fn every_move_is_given_the_reason_the_new_file_shows() {
        let before = Roll::of_voters(&voters(
            &["stays", "quiet", "gone", "staffer", "botlike"],
            &["now-active"],
            &["was-staff (staff)"],
        ));
        let after = Roll::of_voters(&voters(
            &["stays", "fresh", "now-active", "was-staff"],
            &["quiet"],
            &["staffer (staff)"],
        ));
        let mut after = after;
        after.bots = ["botlike".to_string()].into();

        let moved = changes(&before, &after, Which::Voters);
        assert_eq!(moved.joined_count, 3);
        assert_eq!(moved.left_count, 4);
        assert_eq!(
            moved.joined,
            vec![
                Group {
                    reason: "added maintainer",
                    logins: vec!["fresh".into()]
                },
                Group {
                    reason: "now active in the window",
                    logins: vec!["now-active".into()]
                },
                Group {
                    reason: "off the staff list",
                    logins: vec!["was-staff".into()]
                },
            ],
            "the ordinary reason first"
        );
        assert_eq!(
            moved.left,
            vec![
                Group {
                    reason: "no activity",
                    logins: vec!["quiet".into()]
                },
                Group {
                    reason: "no longer a maintainer",
                    logins: vec!["gone".into()]
                },
                Group {
                    reason: "added to the staff list",
                    logins: vec!["staffer".into()]
                },
                Group {
                    reason: "counted as a bot",
                    logins: vec!["botlike".into()]
                },
            ]
        );
    }

    /// A nominee has no maintainer half, so the wording is about activity.
    #[test]
    fn a_nominee_joins_and_leaves_on_activity() {
        let moved = changes(
            &roll(&["stays", "gone"]),
            &roll(&["stays", "new"]),
            Which::Nominees,
        );
        assert_eq!(moved.joined[0].reason, "activity in the window");
        assert_eq!(moved.left[0].reason, "no activity in the window");
    }

    #[test]
    fn the_message_carries_the_numbers_and_names_everyone_who_moved() {
        let before = Snapshot {
            voters: Roll::of_voters(&voters(&["stays", "gone"], &[], &[])),
            nominees: roll(&["stays", "gone", "quiet"]),
        };
        let after = Snapshot {
            voters: Roll::of_voters(&voters(&["stays", "fresh"], &["gone"], &[])),
            nominees: roll(&["stays", "fresh", "quiet"]),
        };
        let text = message(
            Some(&before),
            &after,
            &window(),
            Some("github-audit-tools@abc1234"),
        );
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "Election rolls: 2 voters (+1 / -1), 3 nominees (+1 / -1)"
        );
        assert!(
            lines[0].len() <= 72,
            "the subject stays readable: {}",
            lines[0]
        );
        assert!(text.contains("Window 2025-10-01 to now, from github-audit-tools@abc1234."));
        assert!(text.contains("Voters: 2, was 2 (+1 / -1)"));
        assert!(text.contains("  joined -- added maintainer (1)\n    fresh\n"));
        assert!(text.contains("  left -- no activity (1)\n    gone\n"));
        assert!(text.contains("Nominees: 3, was 3 (+1 / -1)"));
    }

    #[test]
    fn a_run_that_moved_nobody_says_so_in_the_subject() {
        let snapshot = Snapshot {
            voters: Roll::of_voters(&voters(&["stays"], &[], &[])),
            nominees: roll(&["stays"]),
        };
        let text = message(Some(&snapshot.clone()), &snapshot, &window(), None);
        assert_eq!(
            text.lines().next().unwrap(),
            "Election data: 1 voters, 1 nominees, rolls unchanged"
        );
        assert!(text.contains("Voters: 1, unchanged"));
        assert!(!text.contains("joined"));
    }

    #[test]
    fn the_first_publish_claims_nobody_joined() {
        let after = Snapshot {
            voters: Roll::of_voters(&voters(&["alice"], &[], &[])),
            nominees: roll(&["alice", "bob"]),
        };
        let text = message(None, &after, &window(), None);
        assert_eq!(
            text.lines().next().unwrap(),
            "Election rolls: 1 voters, 2 nominees"
        );
        assert!(text.contains("First publish"));
        assert!(!text.contains("joined --"));
    }

    #[test]
    fn a_long_group_wraps_and_then_stops() {
        let many: Vec<String> = (0..260).map(|n| format!("person-{n:03}")).collect();
        let text = wrapped(&many, 4, 72);
        for line in text.lines() {
            assert!(line.len() <= 72, "{line}");
            assert!(line.starts_with("    "), "{line}");
        }
        assert!(text.contains("person-000, person-001,"));
        assert!(text.trim_end().ends_with("... and 60 more"));
    }

    #[test]
    fn one_name_per_line_is_not_padded_into_nonsense() {
        assert_eq!(wrapped(&["alice".to_string()], 2, 72), "  alice\n");
        assert_eq!(wrapped(&[], 2, 72), "");
    }
}
