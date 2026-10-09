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
//! - `rest_client` — the REST (and GraphQL) client `gh-actions-storage` and
//!   `tac-eligibility` share, for the endpoints the GraphQL schema has no
//!   types for.
//! - `storage_*` — Actions artifact storage (`gh-actions-storage`).
//! - `clowarden*`, `tac_*` — the CLOWarden configs, and the TAC election
//!   eligibility phases that read them.

pub mod client;
pub mod rest_client;

pub mod clowarden;
pub mod clowarden_drift;

pub mod tac;
pub mod tac_authors;
pub mod tac_hashes;
pub mod tac_logins;
pub mod tac_voters;

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
pub mod storage_discover;
pub mod storage_measure;
pub mod storage_purge;
pub mod storage_rank;
pub mod storage_results;
pub mod storage_util;
