//! Delete Actions artifacts older than N days.
//!
//! Enforces a low effective retention and reclaims storage now.
//!
//! Why not just change the retention default? Because there is no API for it —
//! the default artifact and log retention period is a UI setting (per org, or
//! capped enterprise-wide by policy). And changing it is not retroactive:
//! artifacts that already exist keep occupying storage until they expire on
//! their original schedule. This deletes them now.
//!
//! Artifacts are listed in full before anything is deleted. Deleting while
//! paginating would shift later pages and silently skip artifacts.

use anyhow::Result;
use futures::stream::{self, StreamExt};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Duration;

use crate::storage_artifacts::{self as artifacts, Artifact};
use crate::storage_client::GhClient;
use crate::storage_discover::{self as discover, Repo};
use crate::storage_results::{self as results, Results};
use crate::storage_util::{epoch_seconds, gib, now_epoch};

pub struct PurgeOptions {
    pub enterprise: Option<String>,
    pub org: Option<String>,
    pub repo: Option<String>,
    pub days: u64,
    pub apply: bool,
    pub sleep_ms: u64,
    pub concurrency: usize,
    pub results_dir: String,
}

pub async fn run(client: &GhClient, opts: PurgeOptions) -> Result<()> {
    let scope = opts
        .repo
        .clone()
        .or_else(|| opts.org.clone())
        .or_else(|| opts.enterprise.clone())
        .unwrap_or_default();
    let results = Results::new(&opts.results_dir, "purge", &scope)?;

    if opts.apply {
        eprintln!(
            ">>> APPLY mode: artifacts older than {} days WILL BE DELETED.",
            opts.days
        );
    } else {
        eprintln!(
            ">>> DRY RUN: showing what would be deleted (older than {} days). \
             Use --apply to delete.",
            opts.days
        );
    }

    let cutoff = now_epoch() - (opts.days as i64) * 86_400;
    let targets = discover::resolve_targets(
        client,
        opts.enterprise.as_deref(),
        opts.org.as_deref(),
        opts.repo.as_deref(),
    )
    .await?;

    let mut total_bytes = 0u64;
    let mut total_count = 0u64;
    let mut failed = 0u64;
    let mut log = String::from("action\trepo\tartifact_id\tsize_bytes\tcreated_at\tname\n");
    // repo -> (bytes, artifacts), for the summary table.
    let mut per_repo: BTreeMap<String, (u64, u64)> = BTreeMap::new();

    for (org, repos) in targets {
        eprintln!("org: {org}");
        for repo in repos {
            let stale = list_stale(client, &repo, cutoff, opts.concurrency).await?;
            for artifact in stale {
                total_bytes += artifact.size_in_bytes;
                total_count += 1;
                let entry = per_repo.entry(repo.full_name()).or_insert((0, 0));
                entry.0 += artifact.size_in_bytes;
                entry.1 += 1;
                let size = gib(artifact.size_in_bytes);
                let label = format!(
                    "{}  #{}  {:.3}GB  {}  {}",
                    repo.full_name(),
                    artifact.id,
                    size,
                    artifact.created_at,
                    artifact.name
                );

                let action = if !opts.apply {
                    "would-delete"
                } else if artifacts::delete(client, &repo.owner, &repo.name, artifact.id).await? {
                    "deleted"
                } else {
                    failed += 1;
                    "FAILED"
                };
                println!("  {action}  {label}");
                let _ = writeln!(
                    log,
                    "{action}\t{}\t{}\t{}\t{}\t{}",
                    repo.full_name(),
                    artifact.id,
                    artifact.size_in_bytes,
                    artifact.created_at,
                    artifact.name
                );

                if !opts.apply {
                    continue;
                }
                if opts.sleep_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(opts.sleep_ms)).await;
                }
            }
        }
    }

    let summary = format!(
        "{} {} artifact(s), reclaiming {:.2} GB",
        if opts.apply { "Deleted" } else { "Would delete" },
        total_count - failed,
        gib(total_bytes),
    );

    results::announce(&results.write("tsv", &log)?);
    results::announce(&results.write(
        "md",
        &markdown_report(&scope, opts.days, opts.apply, &summary, failed, &per_repo),
    )?);

    println!();
    println!("{summary}");
    if failed > 0 {
        println!("{failed} delete(s) failed — re-run to retry them.");
    }
    Ok(())
}

/// A shareable summary of the run, next to the TSV of raw rows.
fn markdown_report(
    scope: &str,
    days: u64,
    apply: bool,
    summary: &str,
    failed: u64,
    per_repo: &BTreeMap<String, (u64, u64)>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Artifact Purge — `{scope}`\n");
    let _ = writeln!(
        out,
        "Mode: **{}** · artifacts older than **{days} days** · live (non-expired) only.\n",
        if apply { "APPLY" } else { "DRY RUN" },
    );
    let _ = writeln!(out, "**{summary}.**\n");
    if failed > 0 {
        let _ = writeln!(out, "> {failed} delete(s) failed — re-run to retry them.\n");
    }

    if per_repo.is_empty() {
        out.push_str("No artifacts matched.\n");
        return out;
    }

    out.push_str("| Repository | Artifacts | Size |\n|---|---|---|\n");
    let mut rows: Vec<_> = per_repo.iter().collect();
    rows.sort_by_key(|(_, (bytes, _))| std::cmp::Reverse(*bytes));
    for (repo, (bytes, count)) in rows {
        let _ = writeln!(out, "| `{repo}` | {count} | {:.3} GB |", gib(*bytes));
    }
    out.push_str("\nPer-artifact detail is in the matching `.tsv`.\n");
    out
}

/// Every live artifact in a repository created before `cutoff`.
async fn list_stale(
    client: &GhClient,
    repo: &Repo,
    cutoff: i64,
    concurrency: usize,
) -> Result<Vec<Artifact>> {
    let first = match artifacts::fetch_page(client, &repo.owner, &repo.name, 1).await? {
        Some(page) => page,
        None => return Ok(Vec::new()),
    };
    if first.total_count == 0 {
        return Ok(Vec::new());
    }

    let mut all = first.artifacts;
    let pages = artifacts::page_count(first.total_count);
    if pages > 1 {
        let rest = stream::iter((2..=pages).map(|page| async move {
            artifacts::fetch_page(client, &repo.owner, &repo.name, page).await
        }))
        .buffer_unordered(concurrency.max(1))
        .collect::<Vec<_>>()
        .await;

        for page in rest {
            if let Some(page) = page? {
                all.extend(page.artifacts);
            }
        }
    }

    let mut stale: Vec<Artifact> = all
        .into_iter()
        .filter(|a| !a.expired)
        .filter(|a| epoch_seconds(&a.created_at).is_some_and(|created| created < cutoff))
        .collect();
    stale.sort_by_key(|a| a.id);
    Ok(stale)
}
