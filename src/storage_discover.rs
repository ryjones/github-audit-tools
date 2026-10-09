//! Discovery of organizations and repositories, via GraphQL.
//!
//! GraphQL is used for everything it can cover. One paginated query per org
//! returns every repository along with its visibility, archived, and fork
//! flags — data the REST equivalent would need extra calls to assemble.
//!
//! Artifacts themselves are not part of the GraphQL schema; see `artifacts`.

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

use crate::rest_client::GhClient;
use crate::storage_util::parse_repo;

const ORGS_QUERY: &str = r#"
query($slug: String!, $cursor: String) {
  enterprise(slug: $slug) {
    organizations(first: 100, after: $cursor) {
      pageInfo { hasNextPage endCursor }
      nodes { login }
    }
  }
}
"#;

const REPOS_QUERY: &str = r#"
query($org: String!, $cursor: String) {
  organization(login: $org) {
    repositories(first: 100, after: $cursor, orderBy: {field: NAME, direction: ASC}) {
      pageInfo { hasNextPage endCursor }
      nodes { name visibility isArchived isFork }
    }
  }
}
"#;

const REPO_QUERY: &str = r#"
query($owner: String!, $name: String!) {
  repository(owner: $owner, name: $name) {
    name visibility isArchived isFork
  }
}
"#;

#[derive(Debug, Clone)]
pub struct Repo {
    pub owner: String,
    pub name: String,
    /// `public`, `private`, or `internal`.
    pub visibility: String,
    pub archived: bool,
    pub fork: bool,
}

impl Repo {
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    /// Visibility plus any noteworthy flags, e.g. `private, archived`.
    /// Visibility matters because public repositories are often discounted to
    /// $0 while still consuming the included-storage quota.
    pub fn labels(&self) -> String {
        let mut labels = vec![self.visibility.clone()];
        if self.archived {
            labels.push("archived".to_string());
        }
        if self.fork {
            labels.push("fork".to_string());
        }
        labels.join(", ")
    }

    fn from_node(owner: &str, node: &Value) -> Option<Self> {
        Some(Self {
            owner: owner.to_string(),
            name: node.get("name")?.as_str()?.to_string(),
            visibility: node
                .get("visibility")
                .and_then(Value::as_str)
                .unwrap_or("UNKNOWN")
                .to_lowercase(),
            archived: node
                .get("isArchived")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            fork: node.get("isFork").and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

/// Every organization in an enterprise. Requires `read:enterprise`.
pub async fn enterprise_orgs(client: &GhClient, slug: &str) -> Result<Vec<String>> {
    let mut orgs = Vec::new();
    let mut cursor: Option<String> = None;

    loop {
        let data = client
            .graphql(ORGS_QUERY, json!({ "slug": slug, "cursor": cursor }))
            .await
            .with_context(|| format!("listing organizations in enterprise {slug:?}"))?;

        let conn = data
            .pointer("/enterprise/organizations")
            .ok_or_else(|| {
                anyhow!(
                    "enterprise {slug:?} not found or not visible to this token \
                     (a `read:enterprise` scope is required)"
                )
            })?
            .clone();

        for node in nodes(&conn) {
            if let Some(login) = node.get("login").and_then(Value::as_str) {
                orgs.push(login.to_string());
            }
        }

        match next_cursor(&conn) {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(orgs)
}

/// Every repository in an organization the token can see.
pub async fn org_repos(client: &GhClient, org: &str) -> Result<Vec<Repo>> {
    let mut repos = Vec::new();
    let mut cursor: Option<String> = None;

    loop {
        let data = client
            .graphql(REPOS_QUERY, json!({ "org": org, "cursor": cursor }))
            .await
            .with_context(|| format!("listing repositories in organization {org:?}"))?;

        let conn = match data.pointer("/organization/repositories") {
            Some(c) => c.clone(),
            // Not an org (or not visible): treat as empty rather than aborting
            // a whole-enterprise scan.
            None => return Ok(repos),
        };

        for node in nodes(&conn) {
            if let Some(repo) = Repo::from_node(org, node) {
                repos.push(repo);
            }
        }

        match next_cursor(&conn) {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(repos)
}

/// A single `OWNER/NAME` repository.
pub async fn single_repo(client: &GhClient, spec: &str) -> Result<Repo> {
    let (owner, name) = parse_repo(spec)?;
    let data = client
        .graphql(REPO_QUERY, json!({ "owner": owner, "name": name }))
        .await
        .with_context(|| format!("looking up repository {spec:?}"))?;

    let node = data
        .get("repository")
        .filter(|v| !v.is_null())
        .ok_or_else(|| anyhow!("repository {spec:?} not found or not visible to this token"))?;

    Repo::from_node(&owner, node).ok_or_else(|| anyhow!("unexpected response for {spec:?}"))
}

/// Repositories to scan, grouped by organization so progress can be reported
/// org by org.
pub async fn resolve_targets(
    client: &GhClient,
    enterprise: Option<&str>,
    org: Option<&str>,
    repo: Option<&str>,
) -> Result<Vec<(String, Vec<Repo>)>> {
    if let Some(spec) = repo {
        let repo = single_repo(client, spec).await?;
        return Ok(vec![(repo.owner.clone(), vec![repo])]);
    }

    let orgs = match (enterprise, org) {
        (_, Some(o)) => vec![o.to_string()],
        (Some(slug), None) => {
            eprintln!("Enumerating organizations in enterprise {slug:?}...");
            let orgs = enterprise_orgs(client, slug).await?;
            eprintln!("  {} organization(s)", orgs.len());
            orgs
        }
        (None, None) => unreachable!("scope is validated by the CLI parser"),
    };

    let mut out = Vec::with_capacity(orgs.len());
    for org in orgs {
        let repos = org_repos(client, &org).await?;
        out.push((org, repos));
    }
    Ok(out)
}

fn nodes(connection: &Value) -> &[Value] {
    connection
        .get("nodes")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn next_cursor(connection: &Value) -> Option<String> {
    let page_info = connection.get("pageInfo")?;
    if page_info.get("hasNextPage")?.as_bool()? {
        page_info.get("endCursor")?.as_str().map(str::to_string)
    } else {
        None
    }
}
