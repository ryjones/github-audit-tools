//! Rendering `hashes.js`, the one file the election page actually reads.
//!
//! The page asks for a GitHub ID, md5s it, and looks the digest up in two
//! arrays. So the file holds no logins -- someone who has the file still cannot
//! list who is eligible without guessing IDs -- and this module's whole job is
//! to hash the way the page does and lay the arrays out the way the file
//! already looks, so a diff shows who changed and nothing else.

use std::collections::BTreeSet;

/// The file, with the two arrays substituted in. Fourteen spaces of indent and
/// the trailing commas are what the committed file has; keeping them means a
/// regenerated file diffs only where the hashes differ.
const TEMPLATE: &str = r#"'use strict';

class Hashes {
    static check(hash) {
        const voter_hashes = [
{voters}
        ];
        const nominee_hashes = [
{nominees}
        ];
        return [(voter_hashes.indexOf(hash) != -1), (nominee_hashes.indexOf(hash) != -1)];
    }
}
if (typeof module != 'undefined' && module.exports) module.exports = Hashes;
"#;

/// What the page checks: md5 of the login as the form sends it, trimmed and
/// lowercased. Sorted and deduplicated, so the file does not leak the order
/// people were found in.
pub fn hashed<'a>(logins: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let digests: BTreeSet<String> = logins
        .into_iter()
        .map(|login| format!("{:x}", md5::compute(login.trim().to_lowercase())))
        .collect();
    digests.into_iter().collect()
}

/// The file as it should be written.
pub fn render(voters: &[String], nominees: &[String]) -> String {
    TEMPLATE
        .replace("{voters}", &array(voters))
        .replace("{nominees}", &array(nominees))
}

fn array(hashes: &[String]) -> String {
    hashes
        .iter()
        .map(|hash| format!("              \"{hash}\","))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The hashes already in a `hashes.js`, so a run can say what it changed.
///
/// This reads the file as text rather than as JavaScript: the arrays are found
/// by their names and the 32-character digests inside them are collected. A
/// file in any other shape yields nothing, which reads as "everything is new".
pub fn previous(text: &str, array_name: &str) -> Vec<String> {
    let Some(after) = text.split_once(&format!("{array_name}_hashes = [")) else {
        return Vec::new();
    };
    let block = after
        .1
        .split_once("];")
        .map(|(head, _)| head)
        .unwrap_or(after.1);
    block
        .split(|ch: char| !ch.is_ascii_hexdigit())
        .filter(|run| {
            run.len() == 32
                && run
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        })
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_login_hashes_the_way_the_form_sends_it() {
        let one = hashed(["ryjones"]);
        assert_eq!(one, [format!("{:x}", md5::compute("ryjones"))]);
        assert_eq!(
            hashed([" RyJones "]),
            one,
            "the page trims and lowercases before hashing, so this must too"
        );
    }

    #[test]
    fn the_same_person_twice_is_one_hash() {
        assert_eq!(hashed(["alice", "Alice", "ALICE "]).len(), 1);
    }

    #[test]
    fn the_rendered_file_round_trips_through_the_reader() {
        let voters = hashed(["alice", "bob"]);
        let nominees = hashed(["bob", "carol"]);
        let text = render(&voters, &nominees);
        assert_eq!(previous(&text, "voter"), voters);
        assert_eq!(previous(&text, "nominee"), nominees);
    }

    #[test]
    fn the_rendered_file_has_the_shape_the_page_already_uses() {
        let text = render(&hashed(["alice"]), &hashed(["bob"]));
        assert!(text.starts_with("'use strict';\n\nclass Hashes {\n    static check(hash) {\n"));
        assert!(
            text.contains("\n              \""),
            "fourteen spaces of indent"
        );
        assert!(text.trim_end().ends_with(
            "if (typeof module != 'undefined' && module.exports) module.exports = Hashes;"
        ));
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn a_file_in_another_shape_reads_as_empty_rather_than_failing() {
        assert!(previous("nothing like it", "voter").is_empty());
        assert!(previous("voter_hashes = [\n  \"short\",\n];", "voter").is_empty());
    }
}
