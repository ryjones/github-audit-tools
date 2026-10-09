//! Authenticated GitHub API client: GraphQL for discovery, REST for artifacts.
//!
//! GitHub's GraphQL schema does not expose Actions artifacts, so this client
//! speaks both protocols:
//!   * GraphQL — enumerating organizations and repositories (see `discover`).
//!   * REST    — listing, sizing, and deleting artifacts (see `artifacts`).
//!
//! Both share one HTTP client, one token, and one retry/rate-limit policy.

use anyhow::{Context, Result, anyhow, bail};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue};
use reqwest::{Response, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;

const USER_AGENT: &str = concat!("gh-actions-storage/", env!("CARGO_PKG_VERSION"));
const MAX_ATTEMPTS: u32 = 5;
/// Never sleep longer than this waiting for a rate limit to reset.
const MAX_RATE_LIMIT_WAIT: Duration = Duration::from_secs(3600);

/// Resolve a token from the environment, falling back to the `gh` CLI.
pub fn resolve_token() -> Result<String> {
    for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Ok(v);
            }
        }
    }

    let out = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .context("GITHUB_TOKEN/GH_TOKEN is unset and running `gh auth token` failed")?;
    if out.status.success() {
        let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }
    bail!("no GitHub token: set GITHUB_TOKEN (or GH_TOKEN), or run `gh auth login`")
}

pub struct GhClient {
    http: reqwest::Client,
    api_root: String,
    wait_on_rate_limit: bool,
}

impl GhClient {
    pub fn new(api_root: &str, token: &str, wait_on_rate_limit: bool) -> Result<Self> {
        let mut headers = HeaderMap::new();
        let mut auth = HeaderValue::from_str(&format!("Bearer {token}"))
            .context("token is not a valid HTTP header value")?;
        auth.set_sensitive(true);
        headers.insert(AUTHORIZATION, auth);
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "x-github-api-version",
            HeaderValue::from_static("2022-11-28"),
        );

        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .default_headers(headers)
            .timeout(Duration::from_secs(60))
            .build()
            .context("building HTTP client")?;

        Ok(Self {
            http,
            api_root: api_root.trim_end_matches('/').to_string(),
            wait_on_rate_limit,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.api_root, path.trim_start_matches('/'))
    }

    /// Send a request, retrying on rate limits and transient server errors.
    async fn execute(&self, rb: reqwest::RequestBuilder) -> Result<Response> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let req = rb
                .try_clone()
                .ok_or_else(|| anyhow!("request body is not retryable"))?;

            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    if attempt >= MAX_ATTEMPTS {
                        return Err(e).context("HTTP request failed");
                    }
                    sleep(backoff(attempt)).await;
                    continue;
                }
            };

            let status = resp.status();
            let throttled =
                status == StatusCode::FORBIDDEN || status == StatusCode::TOO_MANY_REQUESTS;
            if throttled && self.wait_on_rate_limit && attempt < MAX_ATTEMPTS {
                if let Some(delay) = rate_limit_delay(&resp) {
                    eprintln!(
                        "  rate limited ({}); waiting {}s before retry",
                        status.as_u16(),
                        delay.as_secs()
                    );
                    sleep(delay).await;
                    continue;
                }
            }
            if status.is_server_error() && attempt < MAX_ATTEMPTS {
                sleep(backoff(attempt)).await;
                continue;
            }
            return Ok(resp);
        }
    }

    /// GET a REST endpoint. Returns `None` when the resource is absent or the
    /// token cannot see it — mirroring the shell scripts, which skipped repos
    /// with Actions disabled or restricted rather than aborting the scan.
    pub async fn rest_get<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>> {
        let resp = self.execute(self.http.get(self.url(path))).await?;
        let status = resp.status();
        if status == StatusCode::NOT_FOUND
            || status == StatusCode::FORBIDDEN
            || status == StatusCode::UNAUTHORIZED
        {
            return Ok(None);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("GET {path} failed: {status} {}", body.trim());
        }
        let value = resp
            .json::<T>()
            .await
            .with_context(|| format!("decoding response from {path}"))?;
        Ok(Some(value))
    }

    /// DELETE a REST endpoint. Returns whether the delete succeeded; callers
    /// report failures per artifact and keep going.
    pub async fn rest_delete(&self, path: &str) -> Result<bool> {
        let resp = self.execute(self.http.delete(self.url(path))).await?;
        Ok(resp.status().is_success())
    }

    /// Run a GraphQL query and return the `data` object.
    pub async fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
        let body = json!({ "query": query, "variables": variables });
        let resp = self
            .execute(self.http.post(self.url("/graphql")).json(&body))
            .await?;

        let status = resp.status();
        let payload: Value = resp
            .json()
            .await
            .context("decoding GraphQL response")
            .map_err(|e| {
                if status.is_success() {
                    e
                } else {
                    anyhow!("GraphQL request failed: {status}")
                }
            })?;

        if let Some(errors) = payload.get("errors").and_then(Value::as_array) {
            if !errors.is_empty() {
                let msgs: Vec<_> = errors
                    .iter()
                    .filter_map(|e| e.get("message").and_then(Value::as_str))
                    .collect();
                bail!("GraphQL error: {}", msgs.join("; "));
            }
        }
        payload
            .get("data")
            .cloned()
            .ok_or_else(|| anyhow!("GraphQL response contained no data"))
    }

    /// Remaining requests on the core REST rate limit.
    pub async fn core_rate_limit_remaining(&self) -> Result<u64> {
        let v: Option<Value> = self.rest_get("/rate_limit").await?;
        let v = v.ok_or_else(|| anyhow!("could not read /rate_limit"))?;
        Ok(v.pointer("/resources/core/remaining")
            .and_then(Value::as_u64)
            .unwrap_or(0))
    }
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(2u64.saturating_pow(attempt).min(30))
}

/// How long to wait before retrying a throttled response, if the headers say
/// this was a rate limit at all. `None` means "not a rate limit" — e.g. a plain
/// 403 for a repository the token cannot read.
fn rate_limit_delay(resp: &Response) -> Option<Duration> {
    let header = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };

    // Secondary rate limits come with an explicit Retry-After.
    if let Some(secs) = header("retry-after").and_then(|v| v.trim().parse::<u64>().ok()) {
        return Some(Duration::from_secs(secs.saturating_add(1)).min(MAX_RATE_LIMIT_WAIT));
    }

    // Primary rate limit: remaining == 0, wait until the reset epoch.
    let remaining = header("x-ratelimit-remaining")?;
    if remaining.trim() != "0" {
        return None;
    }
    let reset = header("x-ratelimit-reset")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let wait = reset.saturating_sub(now).saturating_add(1);
    Some(Duration::from_secs(wait.max(1)).min(MAX_RATE_LIMIT_WAIT))
}
