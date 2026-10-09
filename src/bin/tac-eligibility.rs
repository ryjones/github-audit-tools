//! Build the TAC election `hashes.js` from the mirrors, the CLOWarden configs
//! and the org-members export.
//!
//! Two lists go into the page, and they come from different places:
//!
//!   nominees -- eligible to run: anyone who authored a commit in the window,
//!               read from the bare mirrors clone-repos.zsh made, with commit
//!               addresses resolved to GitHub logins through the API.
//!   voters   -- eligible to vote: maintainers, meaning everyone a CLOWarden
//!               config puts in a team holding write, maintain or admin on
//!               some repository, plus the organization owners.
//!
//! Four phases, each resumable, each writing its result beside the mirrors so
//! the next one can be re-run on its own:
//!
//!   authors  mirrors            -> repos/authors.json   (no network)
//!   logins   authors.json       -> repos/logins.json    (GitHub API, cached)
//!   voters   configs + people   -> repos/voters.json    (no network)
//!   write    the two lists      -> tac-eligibility-check/hashes.js
//!
//! `repos/` may hold the mirrors of more than one enterprise, so pass --report
//! (YAML or JSON, repeatable) to count only the repositories that report names.
//! Without it, every mirror under repos/ counts.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use futures::stream::StreamExt;

use github_audit_tools::members_model::Report;
use github_audit_tools::rest_client::{self, GhClient};
use github_audit_tools::tac::{self, Window};
use github_audit_tools::{clowarden, tac_authors, tac_hashes, tac_logins, tac_voters};
use tac_authors::Authors;
use tac_logins::{Cache, Logins, Resolved};
use tac_voters::Voters;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Phase {
    Authors,
    Logins,
    Voters,
    Write,
}

/// Build the TAC election hashes.js. Phases: authors, logins, voters, write.
/// All four, in order, by default.
#[derive(Debug, Parser)]
#[command(name = "tac-eligibility", version, long_about = None)]
struct Args {
    /// Run only this phase. Repeatable.
    #[arg(long, value_enum, value_name = "PHASE")]
    phase: Vec<Phase>,

    /// Count only the repositories this gh-enterprise-repos report names, YAML
    /// or JSON. Repeatable.
    #[arg(long, value_name = "FILE")]
    report: Vec<PathBuf>,

    /// The bare mirrors clone-repos.zsh made, and where the intermediate files
    /// go.
    #[arg(long, value_name = "DIR", default_value = "repos")]
    repos: PathBuf,

    /// Start of the activity window.
    #[arg(long, default_value = "2025-07-01")]
    since: String,

    /// End of the activity window (default: now).
    #[arg(long)]
    until: Option<String>,

    /// Credit Co-authored-by trailers as activity too.
    #[arg(long)]
    coauthors: bool,

    /// gh-org-members export, for team membership and owners.
    #[arg(
        long,
        value_name = "FILE",
        default_value = "results/lf-decentralized-trust.gh.yaml"
    )]
    people: PathBuf,

    /// The mirror of the CLOWarden config repositories.
    #[arg(long, value_name = "DIR", default_value = "mirror")]
    clowarden: PathBuf,

    /// The hashes.js to write (default: the checkout in this repository).
    #[arg(
        long,
        value_name = "FILE",
        default_value = "tac-eligibility-check/hashes.js"
    )]
    out: PathBuf,

    /// Mirrors read at a time.
    #[arg(long, value_name = "N")]
    jobs: Option<usize>,

    /// API lookups at a time (GitHub's secondary limit is strict).
    #[arg(long, value_name = "N", default_value_t = 4)]
    api_jobs: usize,

    /// Resolve from the cache only, make no API calls.
    #[arg(long)]
    offline: bool,

    /// Look up again the addresses the API found no account for.
    #[arg(long)]
    retry_unresolved: bool,
}

/// Where each phase leaves its work.
struct Paths {
    authors: PathBuf,
    logins: PathBuf,
    voters: PathBuf,
    cache: PathBuf,
    aliases: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let paths = Paths {
        authors: args.repos.join("authors.json"),
        logins: args.repos.join("logins.json"),
        voters: args.repos.join("voters.json"),
        cache: args.repos.join("cache.json"),
        aliases: args.repos.join("aliases.json"),
    };
    std::fs::create_dir_all(&args.repos)
        .with_context(|| format!("creating {}", args.repos.display()))?;

    let wanted: Vec<Phase> = if args.phase.is_empty() {
        vec![Phase::Authors, Phase::Logins, Phase::Voters, Phase::Write]
    } else {
        args.phase.clone()
    };

    for phase in [Phase::Authors, Phase::Logins, Phase::Voters, Phase::Write] {
        if !wanted.contains(&phase) {
            continue;
        }
        println!("== {} ==", name_of(phase));
        match phase {
            Phase::Authors => authors(&args, &paths)?,
            Phase::Logins => logins(&args, &paths)?,
            Phase::Voters => voters(&args, &paths)?,
            Phase::Write => write(&args, &paths)?,
        }
    }
    Ok(())
}

fn name_of(phase: Phase) -> &'static str {
    match phase {
        Phase::Authors => "authors",
        Phase::Logins => "logins",
        Phase::Voters => "voters",
        Phase::Write => "write",
    }
}

// --------------------------------------------------------------------- authors

fn authors(args: &Args, paths: &Paths) -> Result<()> {
    if !args.repos.is_dir() {
        bail!(
            "no mirrors in {}; run clone-repos.zsh first",
            args.repos.display()
        );
    }
    let wanted = tac_authors::wanted_from_reports(&args.report)?;
    let found = tac_authors::mirrors(&args.repos, wanted.as_deref())?;
    if found.is_empty() {
        let scoped = if wanted.is_some() {
            " for the repositories the reports name"
        } else {
            ""
        };
        bail!(
            "no bare clones under {}{scoped}; run clone-repos.zsh first",
            args.repos.display()
        );
    }
    if let Some(wanted) = &wanted {
        let names: Vec<String> = args
            .report
            .iter()
            .map(|path| {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        let missing = wanted.len().saturating_sub(found.len());
        let tail = if missing > 0 {
            format!("; {missing} are not cloned yet")
        } else {
            String::new()
        };
        println!(
            "{} of the {} repositories in {} are mirrored{tail}",
            found.len(),
            wanted.len(),
            names.join(", ")
        );
    }

    let jobs = args.jobs.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8)
    });
    let sweep = tac_authors::sweep(
        &found,
        &args.since,
        args.until.as_deref(),
        args.coauthors,
        jobs,
    );

    let out = Authors {
        window: window(args),
        reports: args
            .report
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        mirrors: found.len(),
        coauthors: args.coauthors,
        commits: sweep.commits,
        emails: sweep.emails,
    };
    tac::write_json(&paths.authors, &out)?;
    println!(
        "\n{} commits in the window across {} mirrors ({} unreadable), \
         {} distinct addresses -> {}",
        out.commits,
        found.len(),
        sweep.failed,
        out.emails.len(),
        paths.authors.display()
    );
    Ok(())
}

fn window(args: &Args) -> Window {
    Window {
        since: args.since.clone(),
        until: args.until.clone().unwrap_or_else(|| "now".to_string()),
    }
}

// ---------------------------------------------------------------------- logins

fn logins(args: &Args, paths: &Paths) -> Result<()> {
    let authored: Authors = tac::read_json(&paths.authors).ok_or_else(|| {
        anyhow::anyhow!(
            "no {}; run the authors phase first",
            paths.authors.display()
        )
    })?;
    let mut cache = Cache::open(&paths.cache);
    let aliases = tac_logins::aliases(&paths.aliases);
    if !aliases.is_empty() {
        println!(
            "{} mappings from {}",
            aliases.len(),
            file_name(&paths.aliases)
        );
    }

    let mut todo = tac_logins::pending(&authored.emails, &cache, &aliases, args.retry_unresolved);
    let aliased = authored
        .emails
        .keys()
        .filter(|email| aliases.contains_key(*email))
        .count();
    println!(
        "{} addresses, {} already in {}, {} to look up",
        authored.emails.len(),
        authored.emails.len() - todo.len() - aliased,
        file_name(&paths.cache),
        todo.len()
    );
    if !todo.is_empty() && args.offline {
        println!("--offline: leaving {} addresses unresolved", todo.len());
        todo.clear();
    }

    let mut newly_resolved = 0;
    if !todo.is_empty() {
        newly_resolved = resolve_all(args, &authored, &todo, &mut cache)?;
        cache.save()?;
    }

    let (logins, bots, unresolved) = tac_logins::accounts(&authored.emails, &cache, &aliases);
    tac::write_json(
        &paths.logins,
        &Logins {
            window: authored.window,
            logins: logins.clone(),
            bots_dropped: bots.clone(),
            unresolved: unresolved.clone(),
        },
    )?;
    println!(
        "\n{} accounts ({newly_resolved} newly resolved, {} bots dropped), \
         {} addresses with no account -> {}",
        logins.len(),
        bots.len(),
        unresolved.len(),
        paths.logins.display()
    );
    Ok(())
}

/// Ask GitHub about each address, `--api-jobs` at a time.
///
/// One call per address: the commit the authors phase sampled says which
/// account it belongs to. An address with no sample -- a co-author -- or one
/// whose commit GitHub links to nobody can still name its own account, if it is
/// a `noreply` address.
/// Returns how many addresses came back with an account.
fn resolve_all(
    args: &Args,
    authored: &Authors,
    todo: &[String],
    cache: &mut Cache,
) -> Result<usize> {
    let token = rest_client::resolve_token()?;
    let client = GhClient::new("https://api.github.com", &token, true)?;
    let width = todo.len().to_string().len();
    let total = todo.len();
    let answers: Mutex<BTreeMap<String, Resolved>> = Mutex::new(BTreeMap::new());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;

    runtime.block_on(async {
        let mut stream = futures::stream::iter(todo.iter().enumerate())
            .map(|(index, email)| {
                let client = &client;
                let answers = &answers;
                async move {
                    let sample = authored
                        .emails
                        .get(email)
                        .and_then(|author| author.sample.clone());
                    let mut login = None;
                    let mut source = None;

                    if let Some(sample) = &sample {
                        match lookup(client, sample).await {
                            Ok(found) => {
                                if let Some(found) = found {
                                    login = Some(found);
                                    source = Some("api".to_string());
                                }
                            }
                            Err(problem) => {
                                eprintln!(
                                    "[{:>width$}/{total}] {email} FAILED: {problem}",
                                    index + 1
                                );
                                return;
                            }
                        }
                    }
                    if login.is_none()
                        && let Some(found) = tac_logins::login_from_noreply(email)
                    {
                        login = Some(found);
                        source = Some("noreply".to_string());
                    }

                    println!(
                        "[{:>width$}/{total}] {email} -> {}{}",
                        index + 1,
                        login.clone().unwrap_or_else(|| "no account".to_string()),
                        source
                            .clone()
                            .map(|source| format!(" ({source})"))
                            .unwrap_or_default()
                    );
                    answers.lock().expect("answers").insert(
                        email.clone(),
                        Resolved {
                            login,
                            source,
                            via: sample,
                            resolved: tac::today(),
                        },
                    );
                }
            })
            .buffer_unordered(args.api_jobs.max(1));
        while stream.next().await.is_some() {}
    });

    // Into the cache in a stable order, so the periodic saves are predictable.
    let mut resolved = 0;
    for (email, answer) in answers.into_inner().expect("answers") {
        if answer.login.is_some() {
            resolved += 1;
        }
        cache.put(&email, answer)?;
    }
    Ok(resolved)
}

/// `author.login` for one `org/repo@sha`, or `None` when GitHub links the
/// commit to no account.
async fn lookup(client: &GhClient, sample: &str) -> Result<Option<String>> {
    let Some((slug, sha)) = sample.rsplit_once('@') else {
        return Ok(None);
    };
    let commit: Option<serde_json::Value> = client
        .rest_get(&format!("repos/{slug}/commits/{sha}"))
        .await?;
    Ok(commit
        .as_ref()
        .and_then(|commit| commit.get("author"))
        .and_then(|author| author.get("login"))
        .and_then(|login| login.as_str())
        .map(str::to_string))
}

// ---------------------------------------------------------------------- voters

fn voters(args: &Args, paths: &Paths) -> Result<()> {
    if !args.clowarden.is_dir() {
        bail!(
            "no CLOWarden mirror at {}; run ./mirror-clowarden.zsh",
            args.clowarden.display()
        );
    }
    let text = std::fs::read_to_string(&args.people)
        .with_context(|| format!("cannot read {}", args.people.display()))?;
    let export: Report = serde_yaml_ng::from_str(&text)
        .with_context(|| format!("{} is not a gh-org-members export", args.people.display()))?;

    let mirror = clowarden::read(&args.clowarden, |warning| eprintln!("{warning}"))?;
    let skipped = mirror.skipped.len();
    println!(
        "{} CLOWarden configs, {} teams with write or better{}",
        mirror.configs.len(),
        mirror.write_teams().len(),
        if skipped == 0 {
            String::new()
        } else {
            format!(" ({skipped} orgs without a readable config)")
        }
    );

    let (voters, combinations) =
        tac_voters::compute(&mirror, &export, &args.people.display().to_string());
    tac::write_json(&paths.voters, &voters)?;
    println!(
        "{} maintainers ({} bots dropped) -> {}",
        voters.voters.len(),
        voters.bots_dropped.len(),
        paths.voters.display()
    );
    for (combination, count) in combinations {
        println!("  {count:>5}  {combination}");
    }
    Ok(())
}

// ----------------------------------------------------------------------- write

fn write(args: &Args, paths: &Paths) -> Result<()> {
    let voters: Voters = tac::read_json(&paths.voters).ok_or_else(|| {
        anyhow::anyhow!("no {}; run the voters phase first", paths.voters.display())
    })?;
    let nominees: Logins = tac::read_json(&paths.logins).ok_or_else(|| {
        anyhow::anyhow!("no {}; run the logins phase first", paths.logins.display())
    })?;
    let parent = args.out.parent().unwrap_or(std::path::Path::new("."));
    if !parent.is_dir() {
        bail!(
            "no directory for {}; clone tac-eligibility-check into this \
             repository, or name another file with --out",
            args.out.display()
        );
    }

    let voter_hashes = tac_hashes::hashed(voters.voters.values().map(|v| v.login.as_str()));
    let nominee_hashes = tac_hashes::hashed(nominees.logins.values().map(|a| a.login.as_str()));

    let before = std::fs::read_to_string(&args.out).unwrap_or_default();
    let was_voters = tac_hashes::previous(&before, "voter");
    let was_nominees = tac_hashes::previous(&before, "nominee");

    std::fs::write(
        &args.out,
        tac_hashes::render(&voter_hashes, &nominee_hashes),
    )
    .with_context(|| format!("writing {}", args.out.display()))?;

    println!("\nwrote {}", args.out.display());
    for (kind, now, before) in [
        ("voter", &voter_hashes, &was_voters),
        ("nominee", &nominee_hashes, &was_nominees),
    ] {
        let now_set: std::collections::BTreeSet<&String> = now.iter().collect();
        let before_set: std::collections::BTreeSet<&String> = before.iter().collect();
        println!(
            "  {kind}_hashes: {} ({} before, +{} / -{})",
            now.len(),
            before.len(),
            now_set.difference(&before_set).count(),
            before_set.difference(&now_set).count()
        );
    }
    let both: std::collections::BTreeSet<&String> = voter_hashes
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .intersection(&nominee_hashes.iter().collect())
        .cloned()
        .collect();
    println!("  in both lists: {}", both.len());
    println!(
        "\nWindow {} to {}. Nothing was committed; the logins behind the hashes \
         are in {} and {}, which are gitignored.",
        nominees.window.since,
        nominees.window.until,
        file_name(&paths.voters),
        file_name(&paths.logins)
    );
    Ok(())
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}
