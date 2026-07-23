//! Exact live-artifact size for one repository.
//!
//! Unlike `rank`, this paginates every artifact rather than sampling, so it
//! costs one call per 100 artifacts. Use it to confirm a single repository's
//! before/after state around a purge.

use anyhow::Result;
use futures::stream::{self, StreamExt};

use crate::artifacts;
use crate::client::GhClient;
use crate::util::{gib, parse_repo};

pub async fn run(client: &GhClient, spec: &str, concurrency: usize) -> Result<()> {
    let (owner, name) = parse_repo(spec)?;
    let (owner, name) = (owner.as_str(), name.as_str());

    let first = match artifacts::fetch_page(client, owner, name, 1).await? {
        Some(page) => page,
        None => {
            println!("{spec}: no readable Actions artifacts");
            return Ok(());
        }
    };

    let mut live_bytes = first.live_bytes();
    let mut live_count = first.live_count();
    let total = first.total_count;

    let pages = artifacts::page_count(total);
    if pages > 1 {
        let rest = stream::iter((2..=pages).map(|page| async move {
            artifacts::fetch_page(client, owner, name, page).await
        }))
        .buffer_unordered(concurrency.max(1))
        .collect::<Vec<_>>()
        .await;

        for page in rest {
            if let Some(page) = page? {
                live_bytes += page.live_bytes();
                live_count += page.live_count();
            }
        }
    }

    println!(
        "{spec}: {live_count} live artifacts  {:.3} GB  ({total} total incl. expired)",
        gib(live_bytes)
    );
    Ok(())
}
