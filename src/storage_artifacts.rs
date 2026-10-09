//! Actions artifacts, via REST.
//!
//! GitHub's GraphQL schema has no Actions-artifact types: artifacts cannot be
//! listed, sized, or deleted through GraphQL. These endpoints are therefore
//! REST-only, and are the only REST calls this tool makes.

use anyhow::Result;
use serde::Deserialize;

use crate::rest_client::GhClient;

pub const PER_PAGE: u64 = 100;

#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    pub id: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub size_in_bytes: u64,
    #[serde(default)]
    pub expired: bool,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct ArtifactPage {
    /// Artifacts in the repository, expired ones included.
    pub total_count: u64,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
}

impl ArtifactPage {
    /// Bytes held by non-expired artifacts on this page.
    pub fn live_bytes(&self) -> u64 {
        self.artifacts
            .iter()
            .filter(|a| !a.expired)
            .map(|a| a.size_in_bytes)
            .sum()
    }

    /// Number of non-expired artifacts on this page.
    pub fn live_count(&self) -> u64 {
        self.artifacts.iter().filter(|a| !a.expired).count() as u64
    }

    /// Rows returned, expired ones included — the denominator when estimating.
    pub fn rows(&self) -> u64 {
        self.artifacts.len() as u64
    }
}

/// One page of a repository's artifacts. `None` when the repository has no
/// readable Actions data (disabled, restricted, or gone).
pub async fn fetch_page(
    client: &GhClient,
    owner: &str,
    repo: &str,
    page: u64,
) -> Result<Option<ArtifactPage>> {
    let path = format!("/repos/{owner}/{repo}/actions/artifacts?per_page={PER_PAGE}&page={page}");
    client.rest_get(&path).await
}

/// Delete one artifact. Returns whether GitHub accepted the delete.
pub async fn delete(client: &GhClient, owner: &str, repo: &str, id: u64) -> Result<bool> {
    client
        .rest_delete(&format!("/repos/{owner}/{repo}/actions/artifacts/{id}"))
        .await
}

/// Total pages needed to walk `total_count` artifacts.
pub fn page_count(total_count: u64) -> u64 {
    total_count.div_ceil(PER_PAGE)
}
