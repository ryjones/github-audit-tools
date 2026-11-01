//! The voter roll as Helios wants it uploaded.
//!
//! Helios reads a bulk voter upload with `csv.reader` and takes the fields
//! positionally — `voter_type, voter_id, email, name` — so:
//!
//!   * **No header row.** The first field of every line has to be an auth
//!     system Helios knows; a header would make it reject the whole file with
//!     `invalid voter type 'voter_type'`.
//!   * **Two fields per line.** A line with fewer than two is skipped in
//!     silence, which is how a roll loses voters without anyone noticing.
//!   * `email` and `name` are read only for `voter_type: password`, where
//!     Helios makes its own login and has to mail it. LFDT elections
//!     authenticate with GitHub, so Helios matches the voter to the account
//!     that logs in and ignores both columns; they are left off rather than
//!     filled with a commit address that reaches nobody.
//!
//! `voter_id` is therefore the GitHub login, which is what Helios' GitHub auth
//! system stores (`user_data['login']`). It compares them case-insensitively,
//! so the spelling here is the roll's own.

/// The auth system the first column names: Helios' own key for GitHub login.
pub const VOTER_TYPE: &str = "github";

/// `github,<login>` per voter, newline-terminated, no header.
pub fn render<'a>(logins: impl IntoIterator<Item = &'a str>) -> String {
    let mut out = String::new();
    for login in logins {
        let login = login.trim();
        if login.is_empty() {
            continue;
        }
        out.push_str(VOTER_TYPE);
        out.push(',');
        out.push_str(&field(login));
        out.push('\n');
    }
    out
}

/// A GitHub login holds nothing CSV cares about, but the roll is read from
/// files this tool does not write, so a login that would break a line is
/// quoted rather than trusted.
fn field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_per_voter_naming_the_auth_system_and_the_login() {
        assert_eq!(
            render(["RyJones", "alice"]),
            "github,RyJones\ngithub,alice\n",
            "no header, two fields, the roll's own spelling"
        );
    }

    #[test]
    fn an_empty_login_is_not_a_line() {
        // A one-field line is skipped by Helios in silence; never write one.
        assert_eq!(render(["", "  ", "alice"]), "github,alice\n");
    }

    #[test]
    fn a_login_that_would_break_a_line_is_quoted() {
        assert_eq!(render(["a,b"]), "github,\"a,b\"\n");
        assert_eq!(render(["a\"b"]), "github,\"a\"\"b\"\n");
    }
}
