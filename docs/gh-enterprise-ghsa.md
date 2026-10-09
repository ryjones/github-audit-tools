# gh-enterprise-ghsa

List every GitHub Security Advisory (GHSA) across the organizations of a GitHub
enterprise, along with any CVE assigned to each — and, optionally, which repos
have private vulnerability reporting turned off.

A single zsh script over `gh` and `jq`. No dependencies beyond those two.

```
./gh-enterprise-ghsa.zsh -e my-enterprise -R
```

## Why it works this way

**There is no enterprise-level advisory endpoint.** `/enterprises/{slug}/security-advisories`
does not exist — it 404s. So the script expands the enterprise into its member
orgs via the GraphQL `enterprise(slug:).organizations` connection, then walks
[`GET /orgs/{org}/security-advisories`](https://docs.github.com/rest/security-advisories/repository-advisories#list-repository-security-advisories-for-an-organization)
for each one and merges the results.

This is the only path that sees **unpublished** advisories. The global advisory
database (`/advisories`) only contains published GHSAs, which for most orgs is a
small fraction of the total — drafts, triage, and closed advisories never appear
there.

## Install

```sh
git clone git@github.com:ryjones/gh-enterprise-ghsa.git
cd gh-enterprise-ghsa
chmod +x gh-enterprise-ghsa.zsh
```

Requires [`gh`](https://cli.github.com) (authenticated) and [`jq`](https://jqlang.github.io/jq/).

### Token scopes

| Scope / permission | Needed for |
| --- | --- |
| `read:enterprise` | expanding the enterprise into its orgs |
| org owner or security manager | reading each org's advisories |
| repo admin | the `-R` posture probe (repos without it report `unknown`) |

```sh
gh auth refresh -h github.com -s read:enterprise
```

Orgs you cannot read are **warned about and skipped** rather than aborting the
run, so one inaccessible org doesn't sink an enterprise-wide sweep. The exit
code is `1` if anything was skipped.

## Usage

```
Usage: gh-enterprise-ghsa.zsh [options]

  -e SLUG    enterprise slug (default: post-quantum-cryptography-alliance)
  -o ORG     only this org; repeatable. Skips enterprise expansion.
  -s STATE   all | triage | draft | published | closed (default: all)
  -f FORMAT  table | md | csv | tsv | json | ghsa (default: table)
  -c         only advisories that have a CVE assigned
  -R         also report per-repo private vulnerability reporting (PVR) posture
  -D         with -R, list only repos where PVR is disabled
  -A         skip the advisory sweep (use with -R for a posture-only report)
  -a         with -R, include archived repos (excluded by default)
  -j N       parallel PVR probes (default: 8)
  -O FILE    write the report to FILE instead of stdout ("-" means stdout)
  -F         allow -O to overwrite an existing file
  -P         with -f md, emit pure pipe tables instead of full-width HTML ones
  -w N       with -P, truncate summary/description to N chars (default: 90)
  -h         this help
```

### Examples

```sh
# every advisory in the enterprise, as a table
./gh-enterprise-ghsa.zsh -e my-enterprise

# just the published ones with their CVEs
./gh-enterprise-ghsa.zsh -e my-enterprise -s published

# GHSA -> CVE pairs, for feeding another tool
./gh-enterprise-ghsa.zsh -c -f ghsa

# which repos are closed to outside vulnerability reports?
./gh-enterprise-ghsa.zsh -RDA

# full markdown report to a file
./gh-enterprise-ghsa.zsh -R -f md -O reports/security.md

# a single org, skipping enterprise expansion
./gh-enterprise-ghsa.zsh -o my-org
```

### Sample

```
ORG              REPO          GHSA                 CVE  SEVERITY  STATE   PUBLISHED  SUMMARY
my-org  widget  GHSA-aaaa-bbbb-cccc  -    MEDIUM    closed  -          Timing side channel in signature ve…
my-org  gadget  GHSA-dddd-eeee-ffff  -    MEDIUM    closed  -          Out-of-bounds read in the test harn…

── advisory totals ────────────────────────────────
advisories       : 2
with CVE         : 0
without CVE      : 2
by state         : closed=2
by severity      : medium=2
CVEs             : -

ORG              REPO           PVR         ADVISORIES  VISIBILITY  ARCHIVED
my-org  .github        enabled ✓   -           public      -
my-org  widget   enabled ✓   1           public      -
my-org  gadget       DISABLED ✗  -           public      -
```

## Repo posture (`-R`)

**GitHub has no per-repo "security advisories: off" switch.** Any repo admin can
always open a draft advisory. The setting that *can* be off is **private
vulnerability reporting** (PVR): with PVR disabled, outside researchers have no
way to report privately, so the repo only ever gets advisories its own admins
write. That is what `-R` reports, via
[`GET /repos/{owner}/{repo}/private-vulnerability-reporting`](https://docs.github.com/rest/repos/repos#check-if-private-vulnerability-reporting-is-enabled-for-a-repository).

There is no bulk endpoint, so this is one REST call per repo, run `-j` at a time
(default 8). A ~50-repo enterprise takes well under a minute.

Two API behaviors the script normalizes:

- **PVR is a public-repo feature.** The endpoint 404s on private repos even for
  admins, so those are reported as `n/a`, not `unknown`. `unknown` therefore
  means what it says: no admin rights on that repo.
- **Draft advisories create private forks** named `<repo>-ghsa-<id>`. These are
  an artifact of the advisory workflow, not repos anyone configures, so they are
  filtered out of the listing using each advisory's `private_fork` field. This
  filter needs advisory data, so `-A` (posture-only) cannot apply it.

The repo table carries an `ADVISORIES` count so a PVR-disabled repo that already
has advisories stands out, and the footer calls those out explicitly. Under `-A`
there is no advisory data to count, so that column reads `-` throughout.

## Output formats

| Format | Notes |
| --- | --- |
| `table` | aligned columns, summaries truncated. Totals go to **stdout**. |
| `md` | markdown with full-width HTML tables (see below). Totals to stdout. |
| `csv` | full detail — CVSS, CWEs, affected packages, both URLs. Totals to stderr. |
| `tsv` | same columns as `table`, untruncated. Totals to stderr. |
| `json` | the normalized objects. Totals to stderr. |
| `ghsa` | `GHSA-id<TAB>CVE-id` pairs, nothing else. `-R` is suppressed. |

Progress lines and warnings always go to stderr, so `-f csv > out.csv` and
`-f json | jq ...` stay clean. Totals are part of the human-facing report, which
is why `table` and `md` put them on stdout — that way `-O` captures the whole
thing.

With `-R`, `-f json` returns an object rather than a bare array:

```jsonc
{ "advisories": [ ... ], "repos": [ ... ] }   // -R
[ ... ]                                        // without -R
```

With `-R`, `-f csv` emits the advisory CSV, a blank line, then the repo CSV.

### CVE links

Where an advisory has a CVE assigned, the id links to its MITRE CVE Program
record at `https://www.cve.org/CVERecord?id=<CVE>` — in both markdown variants
and in the footer's CVE list. `csv` and `json` carry the same value in a
`cve_url` field, alongside `html_url` for the GHSA itself.

Note that a CVE attached to a **draft** advisory is reserved but not yet
published, so its MITRE record has no content until the advisory goes public
(the page resolves, but the record reads as non-existent). That is expected —
the link is correct, the record just isn't populated yet.

### Markdown table width

A markdown pipe table has **no width control** — it sizes to its content. A table
with long summaries overflows; one with short cells renders as a narrow stub with
dead space beside it. Neither fills the rendering area.

So `-f md` emits **HTML tables** instead: a `<style>` block, `table-layout: fixed`,
and `<colgroup>` percentage widths. Renderers backed by a real WebView — MacDown,
VS Code preview, MkDocs, Docusaurus, Obsidian — apply the CSS, so the table spans
the full width and the summary column wraps rather than forcing horizontal scroll.

The repo table carries a **Description** column for the same reason: without it
the cells are all short and the table renders as a stub regardless of styling.

GitHub is the exception. Its sanitizer strips `<style>` and `style=`, and its CSS
pins tables to `width: max-content`, so **no markup can make a table fill the
width there** — you get a plain, valid, unstyled table. If you are targeting
GitHub, use `-P` for pure pipe tables and `-w` to truncate the summary to
something that fits:

```sh
./gh-enterprise-ghsa.zsh -R -f md -P -w 70 -O reports/security.md
```

### Writing to a file

`-O FILE` stages output through `FILE.partial.$$` and renames on success, so a
run that fails part-way cannot leave a truncated report where a good one was. It
refuses to overwrite an existing file unless `-F` is given, and validates the
target directory *before* spending minutes on API calls.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | success |
| `1` | one or more orgs/repos were skipped, or a usage/validation error |
| `127` | `gh` or `jq` not found |

## License

Apache License 2.0 — see [LICENSE](LICENSE).
