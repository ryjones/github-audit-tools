//! Shared library behind the audit binaries in `src/bin`.
//!
//! One module per concern, prefixed by the tool that owns it, so a flat `src`
//! still says who owns what:
//!
//! - `client` — the GraphQL client `gh-enterprise-repos` and `gh-org-members`
//!   share.
//! - `repos_*` — repository inventory (`gh-enterprise-repos`).
//! - `members_*` — people, teams and audit sweeps (`gh-org-members`,
//!   `gh-org-reports`).
//! - `storage_*` — Actions artifact storage (`gh-actions-storage`), which
//!   carries its own client because it also speaks REST.

pub mod client;

pub mod repos_collect;
pub mod repos_model;
pub mod repos_yaml;

pub mod members_audit;
pub mod members_collect;
pub mod members_logins;
pub mod members_model;
pub mod members_reports;
pub mod members_yaml;

pub mod storage_artifacts;
pub mod storage_client;
pub mod storage_discover;
pub mod storage_measure;
pub mod storage_purge;
pub mod storage_rank;
pub mod storage_results;
pub mod storage_util;
