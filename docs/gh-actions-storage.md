# gh-actions-storage

Measure and reclaim GitHub Actions **artifact storage** across an enterprise, an
organization, or a single repository.

Artifact storage is the usual driver of both the Actions storage bill and the
included-storage quota. This tool ranks repositories by how much **live**
(non-expired) artifact storage they hold, and deletes artifacts past a chosen
age.

## Why "live" artifacts

Expired artifacts keep appearing in the API listing for a while after they stop
occupying billed storage. Counting them badly overstates high-churn,
short-retention repositories — often by several times. Every figure this tool
reports counts non-expired artifacts only.

## Install

```bash
cargo build --release          # all four binaries in this repository
./target/release/gh-actions-storage --help
```

## Authentication

A token is read from `GITHUB_TOKEN`, then `GH_TOKEN`, and finally from
`gh auth token` if the [`gh` CLI](https://cli.github.com) is logged in.

Scopes needed:

| Task | Scope |
|---|---|
| Scan an organization | `read:org`, `repo` |
| Scan an enterprise (`--enterprise`) | `read:enterprise` |
| Delete artifacts (`purge --apply`) | `repo`, or fine-grained Actions read+write |

For GitHub Enterprise Server, point at the instance with `--api-url` or
`GITHUB_API_URL`, e.g. `https://github.example.com/api/v3`.

## Commands

### `rank` — find the biggest consumers

```bash
gh-actions-storage rank --enterprise MY_ENTERPRISE
gh-actions-storage rank --org MY_ORG --top 25 --min-mb 100
```

Prints two rankings — by repository and by organization — and writes the same
report to `results/` as Markdown (including the per-repository scan log, so the
file captures the whole run) and TSV.

```
================ Live artifact storage by REPO ================
  1  ~     6.32 GB  my-org/my-repo                      605 live / 2995 total artifacts  public
  2        0.39 GB  my-org/other-repo                   687 live / 1210 total artifacts  private
----
Shown total: ~6.71 GB across 2 repo(s)   (~ = estimated)
```

Visibility is worth watching: public repositories are often discounted to \$0
while **still consuming the included-storage quota**, so a repository can be
free and still be the reason for a quota warning.

### `purge` — delete artifacts older than N days

Dry run by default; nothing is deleted without `--apply`.

```bash
# preview
gh-actions-storage purge --repo OWNER/NAME --days 3
gh-actions-storage purge --org MY_ORG --days 14

# then delete, pausing between deletes to avoid secondary rate limits
gh-actions-storage purge --org MY_ORG --days 14 --apply --sleep 200
```

Each run writes a Markdown summary and a TSV of every artifact considered to
`results/`, whether it was deleted or merely previewed.

Artifacts are listed **in full before anything is deleted** — deleting while
paginating shifts later pages and silently skips artifacts.

### `measure` — exact size for one repository

```bash
gh-actions-storage measure OWNER/NAME
```

Paginates every artifact instead of sampling, so it costs one call per 100
artifacts. Use it to confirm a repository's before/after state around a purge.

### `rate-limit` — check or wait out the quota

```bash
gh-actions-storage rate-limit
gh-actions-storage rate-limit --wait-for 200
```

Rate limits are normally handled automatically: the tool reads GitHub's
`x-ratelimit-*` and `retry-after` headers and sleeps until the limit resets.
Pass `--no-wait-on-rate-limit` to fail fast instead.

## How ranking works

For each repository, one API call fetches `total_count` plus the newest 100
artifacts. If the repository holds more, up to four further pages are sampled
spread across the range (25/50/75/100%) — expired artifacts cluster in the
**older** pages, so sampling only the newest page would overestimate badly.

Live size is then estimated as:

```
live bytes per sampled artifact × total_count
```

Repositories at or under one page are counted **exactly**. Estimated rows are
marked `~`.

This keeps a scan to 1–5 calls per repository. A repository holding 400,000
artifacts would otherwise cost ~4,000 calls to sum exactly. Estimates are for
prioritization, not penny-accurate billing.

## GraphQL and REST

GraphQL is used for everything it can cover. One paginated query per
organization returns every repository along with visibility, archived, and fork
flags — data the REST equivalent would need extra calls to assemble.

Artifacts are the exception. **GitHub's GraphQL schema has no Actions-artifact
types**, so artifacts cannot be listed, sized, or deleted through GraphQL. Those
calls are REST, and they are the only REST calls this tool makes.

| Operation | Protocol |
|---|---|
| Enterprise → organizations | GraphQL |
| Organization → repositories (+ visibility) | GraphQL |
| Repository → artifacts | REST |
| Delete artifact | REST |
| Rate limit | REST |

## Output

Generated reports land in `results/` (change it with `--results-dir`), named
`<command>-<scope>-<UTC timestamp>` so runs can be compared over time:

```
results/rank-my-org-20260101-120000.md     # shareable report + scan log
results/rank-my-org-20260101-120000.tsv    # raw rows
results/purge-my-org-20260101-120000.md    # purge summary by repository
results/purge-my-org-20260101-120000.tsv   # every artifact considered
```

Reports are Markdown so they can be shared or committed elsewhere as-is; the
TSVs carry the raw rows for further processing. Console output goes to stdout
as usual, so there is no need to redirect it to a log file.

The directory is gitignored — these are scan outputs, not source.

## Reducing storage

In order — the first two stop the bleeding, the third reclaims the backlog.

1. **Lower the default artifact retention.** There is no API for this; it is a
   UI setting. Per organization: *Settings → Actions → General → Artifact and
   log retention*. Enterprise-wide cap: *Policies → Actions*. A low maximum
   (7–14 days) prevents any organization from exceeding it.

2. **Fix heavy-uploading workflows.** Where retention is already churning
   artifacts, volume is the real problem. Add `retention-days:` to
   `actions/upload-artifact` for transient CI output, scope `path:` tightly, and
   reconsider uploading one artifact per matrix leg — those multiply fast.

3. **Purge the existing backlog.** Retention changes are **not retroactive**:
   artifacts that already exist keep occupying storage until they expire on
   their original schedule. `purge` deletes them now.

A one-time purge does not hold. Without a retention default or a scheduled
cleanup job, artifacts accumulate again within days.

## Caveats

- **Snapshot, not a trend.** Expiry is continuous, so figures drift daily.
- **GB means GiB** (÷ 1,073,741,824) throughout.
- **Not covered:** Actions cache (free and auto-evicted, does not count toward
  this quota), GitHub Packages, and Git LFS. Packages and LFS have their own
  storage accounting.
- Repositories with Actions disabled or restricted are skipped silently rather
  than failing a scan.
- Large purges consume the core rate limit (5,000 requests/hour) quickly. Use
  `--sleep` and expect big jobs to run in stages.

## License

Apache-2.0. See [LICENSE](../LICENSE).
