//! Rank repositories by live Actions artifact storage.
//!
//! Counts only LIVE (non-expired) artifacts. Expired artifacts keep appearing
//! in the API listing for a while after they stop being billed, so counting
//! them badly overstates high-churn, short-retention repositories.
//!
//! Method: one call per repository fetches `total_count` plus the newest 100
//! artifacts. If the repository holds more, up to four further pages are
//! sampled spread across the range (25/50/75/100%) — expired artifacts cluster
//! in the OLDER pages, so sampling only the newest page would overestimate.
//! Live size is then ESTIMATED as (live bytes per sampled artifact ×
//! total_count). Repositories at or under one page are counted EXACTLY.
//!
//! That keeps a scan to 1–5 calls per repository even for repositories holding
//! hundreds of thousands of artifacts, where an exact sum would cost one call
//! per 100 artifacts. Estimated rows are marked `~`.

use anyhow::Result;
use futures::stream::{self, StreamExt};
use std::collections::BTreeMap;

use crate::storage_artifacts as artifacts;
use crate::storage_client::GhClient;
use crate::storage_discover::{self as discover, Repo};
use crate::storage_results::{self as results, Results};
use crate::storage_util::gib;
use std::fmt::Write as _;

/// Sampled storage for one repository.
pub struct RepoStat {
    pub full_name: String,
    pub org: String,
    /// Visibility and flags, e.g. `public` or `private, archived`.
    pub labels: String,
    pub live_bytes: u64,
    pub live_count: u64,
    pub total_count: u64,
    /// True when sampling was used rather than an exact sum.
    pub estimated: bool,
}

impl RepoStat {
    fn mark(&self) -> &'static str {
        if self.estimated { "~" } else { " " }
    }
}

pub struct RankOptions {
    pub enterprise: Option<String>,
    pub org: Option<String>,
    pub top: usize,
    pub min_mb: u64,
    pub concurrency: usize,
    pub results_dir: String,
}

pub async fn run(client: &GhClient, opts: RankOptions) -> Result<()> {
    let scope = opts
        .enterprise
        .clone()
        .or_else(|| opts.org.clone())
        .unwrap_or_default();
    let results = Results::new(&opts.results_dir, "rank", &scope)?;

    let targets = discover::resolve_targets(
        client,
        opts.enterprise.as_deref(),
        opts.org.as_deref(),
        None,
    )
    .await?;

    // The scan log is echoed to stderr as it happens and kept for the report,
    // so a long scan's progress survives in the results file too.
    let mut scan_log: Vec<String> = Vec::new();
    let mut stats: Vec<RepoStat> = Vec::new();

    for (org, repos) in targets {
        eprintln!("org: {org}");
        scan_log.push(format!("org: {org}"));
        let measured = stream::iter(
            repos
                .into_iter()
                .map(|repo| async move { measure_repo(client, &repo).await }),
        )
        .buffer_unordered(opts.concurrency.max(1))
        .collect::<Vec<_>>()
        .await;

        for result in measured {
            if let Some(stat) = result? {
                let line = format!(
                    "    {:<48} {:>8.2} GB  ({} live / {} total artifacts, {})",
                    stat.full_name,
                    gib(stat.live_bytes),
                    stat.live_count,
                    stat.total_count,
                    if stat.estimated { "est" } else { "exact" }
                );
                eprintln!("{line}");
                scan_log.push(line);
                stats.push(stat);
            }
        }
    }

    let ranked = rank_and_filter(&stats, opts.min_mb);
    let shown = if opts.top > 0 {
        opts.top.min(ranked.len())
    } else {
        ranked.len()
    };

    print!("{}", text_report(&ranked, shown));
    let report = markdown_report(&ranked, shown, &scope, &scan_log);
    results::announce(&results.write("md", &report)?);
    results::announce(&results.write("tsv", &tsv_report(&ranked))?);
    Ok(())
}

/// Sample one repository's live artifact storage.
async fn measure_repo(client: &GhClient, repo: &Repo) -> Result<Option<RepoStat>> {
    let first = match artifacts::fetch_page(client, &repo.owner, &repo.name, 1).await? {
        Some(page) => page,
        None => return Ok(None),
    };

    let total = first.total_count;
    if total == 0 {
        return Ok(None);
    }

    let mut sampled_bytes = first.live_bytes();
    let mut sampled_live = first.live_count();
    let mut sampled_rows = first.rows();
    let mut estimated = false;

    if total > sampled_rows {
        estimated = true;
        // Expired artifacts cluster in the OLDER pages, so spread the extra
        // samples across the range rather than reading only the newest.
        let last = artifacts::page_count(total);
        let mut seen = vec![1u64];
        for page in [last / 4, last / 2, last * 3 / 4, last] {
            if page <= 1 || seen.contains(&page) {
                continue;
            }
            seen.push(page);
            if let Some(p) = artifacts::fetch_page(client, &repo.owner, &repo.name, page).await? {
                sampled_bytes += p.live_bytes();
                sampled_live += p.live_count();
                sampled_rows += p.rows();
            }
        }
    }

    if sampled_rows == 0 {
        return Ok(None);
    }

    // Live bytes = (live bytes per sampled artifact) × total artifacts.
    let per_artifact = sampled_bytes as f64 / sampled_rows as f64;
    let live_bytes = (per_artifact * total as f64) as u64;
    let live_ratio = sampled_live as f64 / sampled_rows as f64;
    let live_count = (live_ratio * total as f64) as u64;

    if live_bytes == 0 {
        return Ok(None);
    }

    Ok(Some(RepoStat {
        full_name: repo.full_name(),
        org: repo.owner.clone(),
        labels: repo.labels(),
        live_bytes,
        live_count,
        total_count: total,
        estimated,
    }))
}

/// Repositories above `--min-mb`, largest first.
fn rank_and_filter(stats: &[RepoStat], min_mb: u64) -> Vec<&RepoStat> {
    let min_bytes = min_mb.saturating_mul(1024 * 1024);
    let mut ranked: Vec<&RepoStat> = stats.iter().filter(|s| s.live_bytes >= min_bytes).collect();
    ranked.sort_by_key(|s| std::cmp::Reverse(s.live_bytes));
    ranked
}

/// Per-organization totals: (live bytes, live artifacts, repositories).
///
/// Computed over every repository above `--min-mb`, not just the shown top N,
/// so the organization view stays a complete picture.
fn by_org<'a>(ranked: &[&'a RepoStat]) -> Vec<(&'a str, (u64, u64, u64))> {
    let mut totals: BTreeMap<&str, (u64, u64, u64)> = BTreeMap::new();
    for stat in ranked {
        let entry = totals.entry(stat.org.as_str()).or_insert((0, 0, 0));
        entry.0 += stat.live_bytes;
        entry.1 += stat.live_count;
        entry.2 += 1;
    }
    let mut orgs: Vec<_> = totals.into_iter().collect();
    orgs.sort_by_key(|(_, (bytes, _, _))| std::cmp::Reverse(*bytes));
    orgs
}

/// The console report.
fn text_report(ranked: &[&RepoStat], shown: usize) -> String {
    let mut out = String::new();
    let truncated = shown < ranked.len();

    out.push('\n');
    out.push_str("================ Live artifact storage by REPO ================\n");
    let mut shown_bytes = 0u64;
    for (i, stat) in ranked.iter().take(shown).enumerate() {
        shown_bytes += stat.live_bytes;
        let _ = writeln!(
            out,
            "{:>3}  {}{:>9.2} GB  {:<48}  {} live / {} total artifacts  {}",
            i + 1,
            stat.mark(),
            gib(stat.live_bytes),
            stat.full_name,
            stat.live_count,
            stat.total_count,
            stat.labels,
        );
    }
    out.push_str("----\n");
    let _ = writeln!(
        out,
        "Shown total: {}{:.2} GB across {} repo(s)   (~ = estimated)",
        if truncated { "~" } else { "" },
        gib(shown_bytes),
        shown,
    );

    let orgs = by_org(ranked);
    out.push('\n');
    out.push_str("================ Live artifact storage by ORG ================\n");
    let mut total_bytes = 0u64;
    for (i, (org, (bytes, live, repos))) in orgs.iter().enumerate() {
        total_bytes += bytes;
        let _ = writeln!(
            out,
            "{:>3}  {:>9.2} GB  {:<32}  {} live artifacts across {} repo(s)",
            i + 1,
            gib(*bytes),
            org,
            live,
            repos,
        );
    }
    out.push_str("----\n");
    let _ = writeln!(
        out,
        "Total: {:.2} GB across {} org(s)",
        gib(total_bytes),
        orgs.len()
    );
    out
}

/// The same report as Markdown, for sharing. Includes the scan log, so this
/// file alone captures the whole run.
fn markdown_report(ranked: &[&RepoStat], shown: usize, scope: &str, scan_log: &[String]) -> String {
    let mut out = String::new();
    let truncated = shown < ranked.len();

    let _ = writeln!(out, "# GitHub Actions Artifact Storage — `{scope}`\n");
    out.push_str(
        "Live (non-expired) artifact bytes only. Expired artifacts stay visible in the API\n\
         for a while after they stop occupying storage, so counting them overstates\n\
         high-churn repositories.\n\n\
         Sizes marked `~` are **estimated** by sampling up to five pages spread across the\n\
         artifact range; repositories at or under one page are exact. Estimates are for\n\
         prioritization, not penny-accurate billing. GB = GiB (÷ 1,073,741,824).\n\n",
    );

    out.push_str("## By repository\n\n");
    out.push_str("| # | Live size | Repository | Live / total artifacts | Visibility |\n");
    out.push_str("|---|-----------|------------|------------------------|------------|\n");
    let mut shown_bytes = 0u64;
    for (i, stat) in ranked.iter().take(shown).enumerate() {
        shown_bytes += stat.live_bytes;
        let _ = writeln!(
            out,
            "| {} | {}{:.2} GB | `{}` | {} / {} | {} |",
            i + 1,
            if stat.estimated { "~" } else { "" },
            gib(stat.live_bytes),
            stat.full_name,
            stat.live_count,
            stat.total_count,
            stat.labels,
        );
    }
    let _ = writeln!(
        out,
        "\n**Shown total: {}{:.2} GB across {} repo(s).**\n",
        if truncated { "~" } else { "" },
        gib(shown_bytes),
        shown,
    );

    let orgs = by_org(ranked);
    out.push_str("## By organization\n\n");
    out.push_str("| # | Live size | Organization | Live artifacts | Repos |\n");
    out.push_str("|---|-----------|--------------|----------------|-------|\n");
    let mut total_bytes = 0u64;
    for (i, (org, (bytes, live, repos))) in orgs.iter().enumerate() {
        total_bytes += bytes;
        let _ = writeln!(
            out,
            "| {} | {:.2} GB | `{}` | {} | {} |",
            i + 1,
            gib(*bytes),
            org,
            live,
            repos
        );
    }
    let _ = writeln!(
        out,
        "\n**Total: {:.2} GB across {} org(s).**\n",
        gib(total_bytes),
        orgs.len()
    );

    out.push_str("## Scan log\n\n");
    let _ = writeln!(
        out,
        "<details>\n<summary>{} repositories with live artifacts</summary>\n\n```\n{}\n```\n\n</details>",
        ranked.len(),
        scan_log.join("\n"),
    );
    out
}

/// Every ranked repository as TSV, for further processing.
fn tsv_report(ranked: &[&RepoStat]) -> String {
    let mut out =
        String::from("live_bytes\trepo\tlive_count\ttotal_count\testimated\tvisibility\n");
    for stat in ranked {
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}\t{}",
            stat.live_bytes,
            stat.full_name,
            stat.live_count,
            stat.total_count,
            if stat.estimated { "est" } else { "exact" },
            stat.labels,
        );
    }
    out
}
