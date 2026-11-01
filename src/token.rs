//! Which credential a request uses.
//!
//! One token serves nearly every organization. The exception is an
//! organization that has turned classic personal access tokens off, which
//! GitHub reports as a flat refusal rather than as an empty result:
//!
//! ```text
//! `LFDT-Lineth` forbids access via a personal access token (classic).
//! Please use a GitHub App, OAuth App, or a personal access token with
//! fine-grained permissions.
//! ```
//!
//! Every endpoint for that organization answers 403, so it is not a matter of
//! a thinner export: nothing about it can be read at all. One such
//! organization is no reason to make the whole run use a fine-grained token,
//! which would have to be granted, organization by organization, everything
//! the classic one already has.
//!
//! So an organization may name its own token in the environment:
//! `GITHUB_<LOGIN>_TOKEN`, the login uppercased with anything that is not a
//! letter or a digit turned into an underscore -- `GITHUB_LFDT_LINETH_TOKEN`
//! for `LFDT-Lineth`. One name, derived one way, because this is consulted for
//! every organization a run touches: a second guess at the name, such as the
//! login's last segment, would have `GITHUB_LABS_TOKEN` quietly claim
//! `hiero-ledger-labs`, `hyperledger-labs` and two more. A token kept under
//! some other name is reachable through `org-tokens.txt`, which can say
//! `LOGIN = VARIABLE` outright. Whichever variable was used is printed.

/// The variables a run's main credential comes from, in order, before
/// `gh auth token` is tried.
pub const DEFAULT_VARS: &[&str] = &["GITHUB_TOKEN", "GH_TOKEN"];

/// The variable that would hold a token for this organization, if it has one.
pub fn env_vars_for(login: &str) -> Vec<String> {
    let whole = normalize(login);
    if whole.is_empty() {
        return Vec::new();
    }
    vec![format!("GITHUB_{whole}_TOKEN")]
}

/// This organization's own token, if the environment names one, with the
/// variable it came from.
pub fn for_org(login: &str) -> Option<(String, String)> {
    for name in env_vars_for(login) {
        if let Ok(value) = std::env::var(&name) {
            let value = value.trim().to_string();
            if !value.is_empty() {
                return Some((name, value));
            }
        }
    }
    None
}

/// An organization that has to be named for it to be exported at all, with
/// the variable holding its token, if the file said which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    pub login: String,
    pub var: Option<String>,
}

/// The organizations declared in `org-tokens.txt`.
///
/// An enterprise listing cannot be trusted to name them. The GraphQL query
/// returns a null in place of an organization the credential may not read and
/// a message that says why, but never its login -- so an organization that
/// forbids classic tokens is missing from the export with nothing to say which
/// one it was. Naming it in a file is the only way to get it back.
///
/// The file is found at `$GITHUB_ORG_TOKENS_FILE`, else `org-tokens.txt` in
/// the working directory, else beside the tools. One organization per line,
/// `#` for comments, and `LOGIN = VARIABLE` where the variable is not the one
/// `env_vars_for` would guess.
pub fn declared_orgs() -> Vec<Declared> {
    let Some(path) = org_tokens_file() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (login, var) = match line.split_once('=') {
            Some((login, var)) => (login.trim(), Some(var.trim().to_string())),
            None => (line, None),
        };
        if login.is_empty() {
            continue;
        }
        out.push(Declared {
            login: login.to_string(),
            var: var.filter(|v| !v.is_empty()),
        });
    }
    out
}

/// Where that file is, if it is anywhere.
pub fn org_tokens_file() -> Option<std::path::PathBuf> {
    if let Ok(named) = std::env::var("GITHUB_ORG_TOKENS_FILE") {
        let named = named.trim();
        if !named.is_empty() {
            let path = std::path::PathBuf::from(named);
            return path.is_file().then_some(path);
        }
    }
    let here = std::path::PathBuf::from("org-tokens.txt");
    if here.is_file() {
        return Some(here);
    }
    // target/<profile>/<binary> -> the tree the file is tracked in, for a
    // binary run by its path from somewhere else.
    let beside = std::env::current_exe()
        .ok()?
        .parent()?
        .parent()?
        .parent()?
        .join("org-tokens.txt");
    beside.is_file().then_some(beside)
}

/// The token for a declared organization: the variable it named, if it named
/// one, and otherwise the ones `env_vars_for` would guess.
pub fn for_declared(org: &Declared) -> Option<(String, String)> {
    if let Some(var) = &org.var {
        return match std::env::var(var) {
            Ok(value) if !value.trim().is_empty() => Some((var.clone(), value.trim().to_string())),
            _ => None,
        };
    }
    for_org(&org.login)
}

/// Put back the organizations an enterprise listing would not name.
///
/// `unreadable` is how many nulls the listing held. Each declared
/// organization that is not already in `orgs` and does have a token is added,
/// and anything left over is reported: nulls the file does not account for are
/// organizations still missing from the export, which is worth saying out loud
/// rather than leaving to be noticed later.
pub fn add_declared(orgs: &mut Vec<String>, unreadable: usize) {
    let mut added = 0usize;
    for org in declared_orgs() {
        if orgs.iter().any(|o| o.eq_ignore_ascii_case(&org.login)) {
            continue;
        }
        match for_declared(&org) {
            Some((var, _)) => {
                eprintln!("  + {} (declared, token from {var})", org.login);
                orgs.push(org.login.clone());
                added += 1;
            }
            None => {
                let wanted = org
                    .var
                    .clone()
                    .unwrap_or_else(|| env_vars_for(&org.login).join(" or "));
                eprintln!(
                    "  warning: {} is declared in org-tokens.txt but {wanted} is not set; \
                     leaving it out",
                    org.login
                );
            }
        }
    }
    if unreadable > added {
        eprintln!(
            "  warning: {} organization(s) in this enterprise could not be read with this \
             credential and the listing does not say which; name them in org-tokens.txt",
            unreadable - added
        );
    }
}

/// Uppercase, and anything that is not a letter or a digit becomes an
/// underscore, with no run of underscores left doubled and none on the ends.
fn normalize(login: &str) -> String {
    let mut out = String::with_capacity(login.len());
    for ch in login.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_login_names_one_variable_and_only_the_whole_login() {
        assert_eq!(
            env_vars_for("LFDT-Lineth"),
            vec!["GITHUB_LFDT_LINETH_TOKEN".to_string()]
        );
        assert_eq!(
            env_vars_for("hyperledger"),
            vec!["GITHUB_HYPERLEDGER_TOKEN".to_string()]
        );
    }

    #[test]
    fn punctuation_and_case_are_normalized() {
        assert_eq!(
            env_vars_for("LF-Decentralized-Trust"),
            vec!["GITHUB_LF_DECENTRALIZED_TRUST_TOKEN".to_string()]
        );
        assert_eq!(
            env_vars_for("web3j"),
            vec!["GITHUB_WEB3J_TOKEN".to_string()]
        );
        assert_eq!(
            env_vars_for("hiero-ledger-labs"),
            vec!["GITHUB_HIERO_LEDGER_LABS_TOKEN".to_string()]
        );
    }

    #[test]
    fn declarations_drop_comments_and_blank_lines() {
        let file = std::env::temp_dir().join(format!("org-tokens-{}.txt", std::process::id()));
        std::fs::write(
            &file,
            "# a comment\n\nLFDT-Lineth\nOther-Org = GITHUB_SOMETHING_ELSE  # why\n",
        )
        .unwrap();
        // SAFETY: single-threaded test, and the variable is read back at once.
        unsafe { std::env::set_var("GITHUB_ORG_TOKENS_FILE", &file) };
        let declared = declared_orgs();
        unsafe { std::env::remove_var("GITHUB_ORG_TOKENS_FILE") };
        std::fs::remove_file(&file).ok();
        assert_eq!(
            declared,
            vec![
                Declared {
                    login: "LFDT-Lineth".to_string(),
                    var: None
                },
                Declared {
                    login: "Other-Org".to_string(),
                    var: Some("GITHUB_SOMETHING_ELSE".to_string())
                },
            ]
        );
    }

    #[test]
    fn a_login_with_nothing_usable_in_it_names_no_variable() {
        assert!(env_vars_for("---").is_empty());
        assert!(env_vars_for("").is_empty());
    }
}
