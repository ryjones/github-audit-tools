//! gh-actions-storage — measure and reclaim GitHub Actions artifact storage.
//!
//! Artifact storage is the usual driver of the Actions storage bill and of the
//! included-storage quota. This tool ranks repositories by how much LIVE
//! (non-expired) artifact storage they hold, and deletes artifacts past a
//! chosen age.
//!
//! Protocol split: organizations and repositories are discovered over GraphQL;
//! artifacts are listed and deleted over REST, because GitHub's GraphQL schema
//! has no Actions-artifact types.
//!
//! Not covered: Actions cache (free, auto-evicted), GitHub Packages, and Git
//! LFS — each has its own storage accounting.

use github_audit_tools::{
    rest_client as client, storage_measure as measure, storage_purge as purge, storage_rank as rank,
};

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};

use github_audit_tools::rest_client::GhClient;

#[derive(Parser)]
#[command(
    name = "gh-actions-storage",
    version,
    about = "Rank and purge GitHub Actions artifact storage.",
    long_about = "Rank and purge GitHub Actions artifact storage across an enterprise, \
                  an organization, or a single repository.\n\n\
                  Authentication: GITHUB_TOKEN or GH_TOKEN, otherwise `gh auth token`.\n\
                  Scopes: read:org + repo to scan, read:enterprise for --enterprise, \
                  and write access to Actions to delete."
)]
struct Cli {
    /// API root, for GitHub Enterprise Server (e.g. https://github.example.com/api/v3)
    #[arg(
        long,
        global = true,
        env = "GITHUB_API_URL",
        default_value = "https://api.github.com"
    )]
    api_url: String,

    /// Concurrent API requests
    #[arg(long, global = true, default_value_t = 4, value_parser = clap::value_parser!(u16).range(1..=32))]
    concurrency: u16,

    /// Fail instead of waiting when a rate limit is hit
    #[arg(long, global = true)]
    no_wait_on_rate_limit: bool,

    /// Directory for generated reports and logs
    #[arg(long, global = true, value_name = "DIR", default_value = "results")]
    results_dir: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Rank repositories by live artifact storage, largest first
    ///
    /// Repositories holding more than one page of artifacts are sampled and
    /// marked "~"; smaller ones are counted exactly.
    Rank {
        #[command(flatten)]
        scope: ScanScope,

        /// Show only the top N repositories (default: all)
        #[arg(long, default_value_t = 0)]
        top: usize,

        /// Only show repositories holding at least N MB
        #[arg(long, default_value_t = 0)]
        min_mb: u64,
    },

    /// Delete artifacts older than N days (dry run unless --apply)
    Purge {
        #[command(flatten)]
        scope: PurgeScope,

        /// Delete artifacts older than this many days
        #[arg(long, default_value_t = 30)]
        days: u64,

        /// Actually delete. Omit for a dry run.
        #[arg(long)]
        apply: bool,

        /// Pause between deletes, to avoid secondary rate limits
        #[arg(long, value_name = "MS", default_value_t = 0)]
        sleep: u64,
    },

    /// Exactly measure live artifact storage for one repository
    Measure {
        /// Repository as OWNER/NAME
        #[arg(value_name = "OWNER/NAME")]
        repo: String,
    },

    /// Show the core REST rate limit, optionally waiting for it to recover
    RateLimit {
        /// Block until at least this many requests are available
        #[arg(long, value_name = "N")]
        wait_for: Option<u64>,

        /// Seconds between checks while waiting
        #[arg(long, value_name = "SECS", default_value_t = 20)]
        poll: u64,
    },
}

/// Scan scope for read-only commands.
#[derive(Args)]
#[group(required = true, multiple = false)]
struct ScanScope {
    /// Scan every organization in this enterprise
    #[arg(long, value_name = "SLUG")]
    enterprise: Option<String>,

    /// Scan just this organization
    #[arg(long, value_name = "NAME")]
    org: Option<String>,
}

/// Scan scope for purging, which also accepts a single repository.
#[derive(Args)]
#[group(required = true, multiple = false)]
struct PurgeScope {
    /// Scan every organization in this enterprise
    #[arg(long, value_name = "SLUG")]
    enterprise: Option<String>,

    /// Scan just this organization
    #[arg(long, value_name = "NAME")]
    org: Option<String>,

    /// Scan just this repository
    #[arg(long, value_name = "OWNER/NAME")]
    repo: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let token = client::resolve_token()?;
    let client = GhClient::new(&cli.api_url, &token, !cli.no_wait_on_rate_limit)?;
    let concurrency = cli.concurrency as usize;

    match cli.command {
        Command::Rank { scope, top, min_mb } => {
            rank::run(
                &client,
                rank::RankOptions {
                    enterprise: scope.enterprise,
                    org: scope.org,
                    top,
                    min_mb,
                    concurrency,
                    results_dir: cli.results_dir,
                },
            )
            .await
        }

        Command::Purge {
            scope,
            days,
            apply,
            sleep,
        } => {
            purge::run(
                &client,
                purge::PurgeOptions {
                    enterprise: scope.enterprise,
                    org: scope.org,
                    repo: scope.repo,
                    days,
                    apply,
                    sleep_ms: sleep,
                    concurrency,
                    results_dir: cli.results_dir,
                },
            )
            .await
        }

        Command::Measure { repo } => measure::run(&client, &repo, concurrency).await,

        Command::RateLimit { wait_for, poll } => rate_limit(&client, wait_for, poll).await,
    }
}

async fn rate_limit(client: &GhClient, wait_for: Option<u64>, poll: u64) -> Result<()> {
    let Some(target) = wait_for else {
        println!(
            "core rate limit remaining: {}",
            client.core_rate_limit_remaining().await?
        );
        return Ok(());
    };
    if poll == 0 {
        bail!("--poll must be at least 1 second");
    }

    loop {
        let remaining = client.core_rate_limit_remaining().await.unwrap_or(0);
        if remaining >= target {
            println!("core rate limit recovered: {remaining}");
            return Ok(());
        }
        eprintln!("core rate limit at {remaining}, waiting for {target}...");
        tokio::time::sleep(std::time::Duration::from_secs(poll)).await;
    }
}
