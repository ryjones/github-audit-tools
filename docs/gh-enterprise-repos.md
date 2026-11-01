# gh-enterprise-repos

Queries a GitHub enterprise over GraphQL and writes one YAML file per
enterprise listing every repository in every organization it contains. Public,
non-archived repositories by default. `--org` does the same for a single
organization without going through its enterprise.

Companion to [`gh-org-members`](gh-org-members.md), which exports the people
instead.

Works against github.com and GitHub Enterprise Server.

## Build

```sh
cargo build --release          # all four binaries in this repository
./target/release/gh-enterprise-repos --help
```

## Use

```sh
export GITHUB_TOKEN=…            # needs read:org and read:enterprise

# one enterprise → ./acme-inc.yaml
gh-enterprise-repos -e acme-inc

# several enterprises, one file each, into a directory
gh-enterprise-repos -e acme-inc -e acme-research -d results

# a single file, or stdout
gh-enterprise-repos -e acme-inc -o acme.yaml
gh-enterprise-repos -e acme-inc -o - | yq '.repositories[].full_name'

# everything, including private, internal and archived repositories
gh-enterprise-repos -e acme-inc --visibility all --archived include

# one organization → ./acme-labs.yaml, with the teams that reach each repository
gh-enterprise-repos --org acme-labs --teams

# repositories no team has access to
gh-enterprise-repos --org acme-labs --visibility all --archived include --teams -o - \
  | yq '.repositories[] | select(.teams == null) | .full_name'

# GitHub Enterprise Server
gh-enterprise-repos --hostname ghe.example.com -e acme-inc
```

### Options

| Flag | Meaning |
| --- | --- |
| `-e, --enterprise <SLUG>` | Enterprise slug; repeatable, one YAML file each |
| `--org <LOGIN>` | Organization login, instead of or alongside `-e`; repeatable, one YAML file each |
| `-d, --output-dir <DIR>` | Where `<enterprise>.yaml` or `<org>.yaml` is written (default `.`) |
| `-o, --output <FILE>` | Write one export to this file instead; `-` is stdout |
| `--visibility <V>` | `public` (default), `private`, `internal`, `all` |
| `--archived <A>` | `exclude` (default), `include`, `only` |
| `--forks <F>` | `include` (default), `exclude`, `only` |
| `--topics` | Include topics, which cost a nested lookup per repository |
| `--teams` | Include the teams with access to each repository, which costs a walk of every team |
| `--hostname <HOST>` | GitHub Enterprise Server host, e.g. `ghe.example.com` |
| `--api-url <URL>` | Full GraphQL endpoint, if it is not `https://<host>/api/graphql` |
| `--concurrency <N>` | Organizations queried at once (default 3, max 16) |
| `--max-retries <N>` | Retries per request (default 5) |
| `--batch-size <N>` | Items per cursor fetch (default 100, max 100) |

The token is read from `GITHUB_TOKEN`, falling back to `GH_TOKEN`, and then to
`gh auth token` if the `gh` CLI is logged in. The CLI's credential is worth
preferring: it carries the SSO authorizations and organization grants that a
hand-made PAT has to be given one organization at a time, and an enterprise
listing made with a token that cannot see an organization leaves that
organization out without an error. The startup line names which of the three
the run used.

## Output

```yaml
source:
  api_url: https://api.github.com/graphql
  authenticated_as: alice
  token_scopes: read:org, repo
  enterprise: acme-inc
  filters:
    visibility: public
    archived: exclude
    forks: include
organizations:
  - acme-labs
  - acme-platform
  - acme-tools
totals:
  organizations: 3
  repositories: 42
  organizations_without_repositories: 0
repositories:
  - org: acme-labs
    name: widget-kit
    full_name: acme-labs/widget-kit
    url: https://github.com/acme-labs/widget-kit
    description: A toolkit for building widgets
    visibility: PUBLIC
    archived: false
    fork: false
    default_branch: main
    language: Rust
    license: Apache-2.0
    stars: 120
    forks: 52
    created_at: 2024-09-17T07:53:35Z
    updated_at: 2026-07-25T11:47:32Z
    pushed_at: 2026-06-05T10:19:46Z
```

The file is written the way `yq .` prints it — sequences indented under their
key, quoting only where a scalar needs it — so running `yq` over the output is
a no-op and re-exports diff cleanly against each other.

Ordering is deterministic: repositories by org login then repository name (both
case-insensitive). `template` and `empty` appear only when true; a field the
token could not read, or that the repository does not have, is absent rather
than null. `topics` is only fetched with `--topics`, and is then sorted by name. `license` is the SPDX id, falling back to the
license name when GitHub reports `NOASSERTION`.

With `--org` the file is the same shape, except that `source` names an
`organization` instead of an `enterprise` and `organizations` holds that one
login.

## Teams

`--teams` adds the teams that have access to each repository and the permission
each one holds — `admin`, `maintain`, `write`, `triage` or `read` — in slug
order:

```yaml
    teams:
      widget-admins: admin
      widget-maintainers: maintain
```

A repository no team reaches has no `teams` key, and
`totals.repositories_without_teams` counts those. That total is only present
with `--teams`, which is how a file with no `teams` keys says whether nobody has
access or nobody asked.

GraphQL has no teams field on a repository, so the run walks every team in the
organization and inverts each one's repository list; the cost grows with the
number of teams, not repositories. This is team access only: people added to a
repository directly, and outside collaborators, are not teams and do not appear.
Secret teams are visible to their members and to organization owners, so a
listing made by anyone else can show a repository as teamless when it is not. If
the teams of an organization cannot be read, that organization fails rather
than exporting with every repository looking unowned.

Organizations with no matching repositories still appear under
`organizations:`. An organization that could not be read at all is listed there
too, plus under `organizations_without_repository_data`, so a permissions gap
does not read as an empty org.

## Drift against a CLOWarden config

`drift.zsh` compares an organization's export with the
[CLOWarden](https://github.com/cncf/clowarden) `config.yaml` meant to describe
it and writes what differs to `DRIFT.md` beside the export. It needs `yq` and
`jq`, reads the two files, and never talks to GitHub.

```sh
gh-enterprise-repos --org acme-labs --visibility all --archived include --teams -d results
./drift.zsh -c ../acme-labs/governance/config.yaml      # → results/DRIFT.md
```

The report opens with the counts, then lists: active repositories the config
does not manage, private and internal repositories, archived repositories,
active repositories no team reaches, managed repositories whose visibility or
team permissions differ from the config, repositories the config names that
GitHub does not have, and teams on one side but not the other. The export has to
be made with `--teams`; one made under a filter is accepted, and the report says
its counts are partial.

| Flag | Meaning |
| --- | --- |
| `-c CONFIG` | The CLOWarden config to compare against, or set `CLOWARDEN_CONFIG` |
| `-o OUT` | Report to write (default `DRIFT.md` beside the export) |
| `EXPORT` | The export to read (default: the only `.yaml` in `results/`) |

## Cloning what a report lists

`clone-repos.zsh` bare-clones every public, non-archived repository in a report
into `<root>/<org>/<name>`, over https. The report can be YAML or JSON, and
several can be given at once: they are merged, and a repository two of them both
name is cloned once. It needs `git`, `yq` and `jq`.

The whole arrangement is three stages — **github.com → the cache → Forgejo**.
The middle one is a tree of bare mirrors that nothing else owns: everything in
it can be fetched again, so it is a cache, and keeping one tree across runs is
what makes the second run a fetch instead of a clone. Both scripts resolve its
root the same way, so they always work on the same tree:

1. `-d DIR`, if given;
2. `$GITHUB_MIRROR_DIR`, if set;
3. `/Volumes/graid/github-mirror`, when that array is mounted;
4. `repos/` beside the script otherwise — gitignored, and the fallback on a
   machine without the array.

Forgejo is fed from the cache, never from github.com directly, which is why
`--forgejo` needs the trees and blobs the mirror holds.

```sh
gh-enterprise-repos --enterprise lf-decentralized-trust -d reports
./clone-repos.zsh reports/lf-decentralized-trust.yaml
./clone-repos.zsh reports/*.yaml            # several enterprises into one tree
```

`--org LOGIN` skips the report: it runs `gh-enterprise-repos` for that
organization itself, into a listing inside the run's temporary directory, and
goes on as if that listing had been given. Repeat it for several organizations,
and mix it with reports named on the command line if you like.

```sh
./clone-repos.zsh --forgejo --org LFDT-Panurus
./clone-repos.zsh --org LFDT-Panurus --org LFDT-IPRAM -n
```

It uses `target/release/gh-enterprise-repos`, then the debug build, then
whatever is on `PATH`, and the listing's own defaults are the ones this script
wants anyway: public, not archived, forks included. The listing itself is not
kept — `gh-enterprise-repos --org LOGIN -d reports` is still how to keep one,
and an enterprise still goes through a report, since `--org` takes one
organization at a time.

The clones are mirrors meant for counting activity per account over a time
period, so they are fetched with `--filter=tree:0`: every commit with its
author, committer and dates, and no file content. That makes `git log` work
offline and keeps the whole enterprise small; `--full` clones trees and blobs
too, for anything that needs a diff.

Re-running fetches into the clones that are already there, so an interrupted run
resumes and a stale mirror catches up. Each clone is made under a `.partial`
name and renamed when git is done, so a half-clone is never mistaken for a
finished one. Credential prompts are disabled: a repository that was renamed,
deleted or made private since the report fails in its own line rather than
waiting for a password. Failures are summarized at the end and set the exit
status; the other clones still happen.

A repository with no commits yet is reported as the empty repository it is —
`empty repository: no branches or tags to push` — and counted as such in the
summary. There is nothing to clone and nothing to push, and no repository is
created on Forgejo for it; it is a fact about the upstream, not a failure.

A clone of a large repository, the refetch of a filtered one, or a push of a
few hundred tags can take minutes with nothing to show for it. Every
`--progress` seconds, 10 by default, one line names the repositories that are
still working and what they are doing:

```
  … still working: trustoverip/spec-up-t (pushing every branch and tag, 2m41s), trustoverip/spec-up-t-website (cloning from github.com, 24s)
```

Only the ones slower than that interval are named, longest first and three at a
time, so a run of small repositories prints nothing extra. Forgejo writes a
release row per tag, so the first push of a tag-heavy repository is usually the
slowest thing in a run — `trustoverip/spec-up-t`, with 29 branches and 216
tags, took four minutes while the other 62 repositories finished in twenty
seconds.

A repository named `.github` is cloned as `_github`, since a dotted directory
hides itself from globs and most tooling. On Forgejo it keeps its real name.

| Flag | Meaning |
| --- | --- |
| `--org LOGIN` | List this organization with `gh-enterprise-repos` and clone what it names; repeatable, and no `REPORT` is then needed |
| `-d DIR` | Where the clones go (default: the mirror cache, `/Volumes/graid/github-mirror` on this machine) |
| `-j JOBS` | Clones at a time (default: the machine's CPU count) |
| `--full` | Clone trees and blobs too, not just commit history |
| `--forgejo` | Also create each organization and repository on a Forgejo instance and push every branch and tag into it; implies `--full` |
| `--forgejo-url URL` | The instance (default `$FORGEJO_URL`, else `https://forgejo.lan.ryjones.dev`) |
| `--progress SECS` | How often to name the repositories that are still working (default 10; `0` says nothing until each one finishes) |
| `-n` | List what would be cloned, with its destination, and stop |
| `--print-root` | Print the mirror root a run would use, and stop — this script owns that decision, and `election.zsh` asks it rather than keeping its own copy |
| `REPORT` | The reports to read, YAML or JSON; several are merged, and so are any `--org` listings |

### Private and internal repositories

Public repositories by default, which is also what a default report holds.
`--visibility` takes the rest, in the same four words `gh-enterprise-repos`
uses:

```sh
./gh-enterprise-repos -e lf-decentralized-trust --visibility all   # the report
./clone-repos.zsh --visibility all results/*.yaml                  # the clones
./clone-repos.zsh --visibility private results/*.yaml              # only those
```

Both halves have to ask for it: a report made without `--visibility` holds
public repositories only, so `--visibility all` over that report finds exactly
what `--visibility public` would.

A private or an internal repository is cloned over **ssh**, with the key
github.com already has — `ssh://git@github.com/<org>/<name>.git` — while public
ones stay on https. github.com will not serve a private repository over https
without a credential, and nothing here will put a token in a URL, where
`remote.origin.url` on disk and `ps` would both hold it. `GIT_TERMINAL_PROMPT=0`
and ssh's `BatchMode=yes` keep a missing key a failure rather than a prompt
that waits forever in a parallel run.

Two things worth knowing before mirroring a whole enterprise this way:

- **Repositories on the instance are created the same as ever** — the push does
  not mark a mirror of a private repository private on Forgejo. Whoever can
  reach the instance can read it.
- **GitHub's per-advisory forks are never mirrored.** Drafting a security
  advisory makes a temporary private fork named for it,
  `<repo>-ghsa-xxxx-xxxx-xxxx`, to write the fix in. It is nobody's repository
  — not configured, not maintained — and what it holds is an unpublished fix to
  someone else's advisory, which is not ours to copy anywhere. They report
  `fork: false`, so only the name gives them away, and they are the bulk of
  what `--visibility private` would otherwise find: 33 of the 56 private
  repositories in the LFDT enterprise on 2026-10-22, against 23 real ones. The
  count left out is reported rather than dropped quietly:

  ```
  23 private, non-archived repositories (33 advisory private forks left out)
  from lf-decentralized-trust.yaml, … would be mirrored into …
  ```

## A Forgejo copy of the enterprise

`--forgejo` gives a Forgejo instance a copy of everything the report names:
the organization, created public if it is not there, the repository inside it,
and then every branch and every tag, pushed from the clone on disk.

```sh
./clone-repos.zsh --forgejo reports/lf-decentralized-trust.yaml
./clone-repos.zsh --forgejo --org LFDT-Panurus     # one org, without a report
./clone-repos.zsh --forgejo -n reports/*.yaml      # the plan, and nothing else
```

These are **not** Forgejo mirrors. A pull mirror would have the instance fetch
from github.com on a schedule, keep the repository read-only, and stop being a
copy the day the network, the token or the upstream repository goes away. Each
repository here is an ordinary Forgejo repository, filled from the clone, which
stands on its own afterwards and can be pushed to, branched and forked like any
other.

What is and is not copied:

- **Every branch and every tag**, forced, with anything the upstream no longer
  has pruned off the Forgejo side on the next run.
- **Not `refs/pull/*`.** GitHub advertises a ref per pull request — nearly five
  thousand of them on `hyperledger/fabric` alone — and Forgejo keeps its own
  pull requests in that same namespace.
- **The default branch**, set from the clone's `HEAD`, which is what GitHub
  advertises as the default; the report's `default_branch` is the fallback.
- **The description**, from the report, when the repository is created. It is
  not written again afterwards, so an edit on Forgejo survives later runs.
- **The git-lfs objects**, fetched from github.com and uploaded to the
  instance before the refs that point at them. See below.
- **No releases, no issues, no pull requests, no wiki.** A bare clone holds
  none of those; this copies git, not GitHub.

`--forgejo` implies `--full`, because a `--filter=tree:0` clone has no trees or
blobs to push, only a promise to fetch them from github.com later. A clone that
was already fetched that way is refetched whole the first time it is pushed,
which is the slow part of a first `--forgejo` run over a tree cloned earlier.

Two credentials, for two jobs:

| What | How | Where it comes from |
| --- | --- | --- |
| Creating the org, the repo, and setting the default branch | REST, over https | `FORGEJO_TOKEN`, else the file named by `FORGEJO_TOKEN_FILE` (default `~/.config/forgejo/token.txt`) |
| The push itself | git, over ssh | the ssh key registered on the instance; `FORGEJO_SSH_USER` is the user, `git` by default |

The token needs `write:organization` and `write:repository`. It is never passed
as an argument — it goes into a `0600` curl config file for the run — because an
argument is in the process list for every account on the machine. The push is
over ssh rather than https for the same reason.

An organization that cannot be created is reported, and its repositories are
left out of the run rather than failing one by one; if none of them can be
created, the run stops before cloning anything.

### Repositories that use git-lfs

A `git clone --mirror` copies a git-lfs pointer blob — a hundred-odd bytes
naming a hash — and not one byte of the file it stands for. A push of such a
clone to an instance that speaks git-lfs, as Forgejo does, is not merely
incomplete: git-lfs refuses the whole push, every ref with it, and what is left
behind is a repository created on the instance with nothing in it.

```
Git LFS upload failed:
  (missing) test-data/precomputed_shares.json (86aa36e8…)
hint: Your push was rejected due to missing or corrupt local objects.
error: failed to push some refs to 'ssh://…/LFDT-Lockness/cggmp21.git'
```

So a repository holding pointers gets two transfers the others do not: the
objects down from github.com into the clone, then up to the instance, in that
order, so the copy there is never briefly one whose files cannot be checked
out. Both show up as their own stage in the progress lines, and the run says
how many objects it moved:

```
[12/58] LFDT-Lockness/cggmp21 updated, 1 ref fetched, forgejo 50 refs updated, 1 lfs object mirrored
   40s    … still working: LFDT-Lockness/cggmp21 (fetching 1 git-lfs object from github.com, 1m20s)
```

It is worth knowing what this costs. The scan that finds the pointers walks
every ref, and so does the upload, which is slow out of proportion to the bytes
— a 110 KB payload across 248 refs took 78 seconds. The payload itself can be
large: of the 648 repositories in the LFDT mirror only four use git-lfs at all,
but one of those four carries 5.5 GB in 112 objects. Repositories with no
pointers pay nothing but the scan, which for them is instant.

This is done here rather than left to git-lfs's own `pre-push` hook. That hook
is ambient — `git lfs install` writes it into whatever `core.hooksPath` points
at, so whether a mirror push carried its objects would depend on how the
machine running the script happened to be set up — and when an object is
missing it takes the ref push down with it. `fj_push` passes `--no-verify` for
the same reason: a mirror push has no business running hooks. If `git-lfs` is
not installed at all, both scripts say so at the start of a `--forgejo` run,
because nothing else could then tell that a repository had arrived as pointers
with nothing behind them.

### What is never pushed over

The push is forced and prunes, which is right for a copy of GitHub and wrong
for anything else. Two things stop it:

- **`forgejo-skip.txt`**, beside the scripts: one `ORG/NAME` per line,
  case-insensitive, `#` for comments. A repository named there is still cloned
  from github.com, but never pushed, because the copy on the instance is the
  maintained one rather than a copy. `FORGEJO_SKIP_FILE` names a different
  file. Both scripts print how many repositories the list holds.
- **A copy that has moved on its own.** Before each push, `git ls-remote`
  against the instance is checked against the clone, and if any branch or tag
  there points at an object the clone does not have, that repository is left
  alone with the ref named:

  ```
      LFDT-way/discord-stats: refs/heads/main on the instance is not in the clone; not pushing over it
  ```

Both are counted as `left alone` in the summary rather than as failures. The
reason both exist: on 2026-10-21 a run over `LFDT-way` forced
`LFDT-way/discord-stats` back to its github.com head and threw away that day's
generated reports, which another job had pushed there an hour earlier. The
list says which repositories are like that; the `ls-remote` check catches the
ones nobody thought to list.

## Updating the clones

`update-repos.zsh` is the second run and every run after it: it fetches each
clone from github.com again and pushes what changed on to Forgejo.

```sh
./update-repos.zsh                          # every clone in the mirror tree
./update-repos.zsh hyperledger              # one organization
./update-repos.zsh hyperledger/fabric trustoverip
./update-repos.zsh --local-only             # the clones only, leave Forgejo
./update-repos.zsh --no-fetch               # Forgejo only, from what is on disk
```

It reads no report. What it updates is what is on disk, found by walking
`DIR/<org>/<name>` for bare clones, and each clone's own `remote.origin.url`
says which GitHub repository it is — so one stored as `_github` is still pushed
to Forgejo as `.github`. A repository that has appeared on GitHub since the
last run is therefore not here: refresh the report and re-run
`clone-repos.zsh`, which skips what it already has.

| Flag | Meaning |
| --- | --- |
| `-d DIR` | The clone tree to update (default: the same mirror cache `clone-repos.zsh` writes) |
| `-j JOBS` | Repositories at a time (default: the machine's CPU count) |
| `-n` | List what would be updated, and which clones would be refetched whole, then stop |
| `--local-only` | Fetch the clones from github.com and stop; no Forgejo, and no refetch of a filtered clone |
| `--no-fetch` | Push to Forgejo from the clones as they are; no fetch |
| `--forgejo-url URL` | The instance (default `$FORGEJO_URL`, else `https://forgejo.lan.ryjones.dev`) |
| `--progress SECS` | How often to name the repositories that are still working (default 10; `0` says nothing until each one finishes) |
| `SLUG` | Limit to these repositories, each an org or an `org/name`; either the real name or the name on disk matches, and case is ignored |

Each line says what happened to one repository — how many refs the fetch
brought, how many the push moved, whether the clone had to be refetched whole,
and how big it is now. A clone with no branches and no tags is reported as the
empty repository it is, and `forgejo-skip.txt` and the `ls-remote` check above
apply here too: this is the script that runs often, so it is the one that would
otherwise push over a copy that has moved on its own. Failures are summarized at the end and set the exit
status, with the fetch and the push counted separately: a push that fails does
not cost the fetch that succeeded.

`forgejo-lib.zsh`, beside both scripts, holds what they share: the API calls,
the push, the refetch, the two guards above, and the progress bookkeeping. It
is sourced, not run.

## TAC election hashes

The repositories this tool lists, cloned by `clone-repos.zsh`, are what the LFDT
TAC election eligibility page counts as activity. That is its own tool and its
own page: [docs/tac-eligibility.md](tac-eligibility.md).

## Whose view a listing is

An organization the token cannot see is not an error. `enterprise.organizations`
simply does not return it, the listing covers the organizations that came back,
and nothing in the file says one is missing. An organization it can see but
cannot see *into* is worse: that one answers with an empty repository list, so
it appears in `organizations` holding nothing at all.

A third kind is an organization that refuses the credential outright, by having
turned classic personal access tokens off. It comes back as a null in
`enterprise.organizations`, counted but never named, and reading it needs a
token of its own — see
[an organization with its own credential](gh-org-members.md#an-organization-with-its-own-credential)
and `org-tokens.txt`.

`authenticated_as` and `token_scopes` record whose view produced the file —
`token_scopes` is absent for fine-grained PATs and App tokens, which do not
report scopes. `totals.organizations_without_repositories` counts the
organizations that answered and held nothing the filters kept.

Under a filter that count is unremarkable: most organizations have no internal
repositories. When a run filters *nothing* out — `--visibility all --archived
include --forks include` — and at least three organizations, and at least 10% of
them, still come back empty, the run prints a warning and records it under
`notes`:

```yaml
notes:
  - "organizations_without_repositories: 6 of 54 organizations (11.1%) held no
    repository, although this run filtered none out. An organization the token
    cannot see into answers with an empty list rather than an error, so check
    these against the enterprise before reading them as empty."
```

`gh auth token` is usually the credential that reaches everything, since it
carries the SSO authorizations a PAT has to be granted per organization.

## Behavior worth knowing

- **The filter is applied twice.** `visibility`, `isArchived` and `isFork` are
  sent as query arguments so GitHub does the work, and each returned repository
  is re-checked locally, so a server that ignores an argument cannot widen the
  result set. A null `isArchived`/`isFork` reads as false; an unreadable
  visibility keeps the repository rather than silently dropping it.
- **Cursor pagination throughout.** Every connection advances by
  `after: <endCursor>`; there are no page numbers or offsets. Topics, when
  requested, are capped at 20 per repository by GitHub, so a single nested fetch
  is complete.
- **Rate limits.** The client tracks the `x-ratelimit-*` headers and waits for
  the reset before spending the last of the budget, honors `Retry-After`,
  recognizes secondary rate limits and `RATE_LIMITED` responses on an otherwise
  successful request, and backs off exponentially on 5xx and timeouts. A
  hostname that does not resolve fails immediately instead of retrying.
- **Partial results beat no results.** One unreadable organization is reported
  on stderr and the enterprise still exports; one unreadable enterprise does not
  stop the others. The exit status is non-zero only when every organization in
  an enterprise fails, or every enterprise fails.

## Tests

```sh
cargo test
```

Unit tests cover the filter (argument mapping, local re-check, unreadable
fields), report assembly (ordering, unreadable orgs, topic sorting, license
fallback, omitted fields, team attachment) and the backoff/reset arithmetic. They make no
network calls.

`results/` is gitignored and holds YAML captured from real runs, kept so the
output shape can be inspected without re-spending API quota. A `.batch3.yaml`
capture is the same export fetched with `--batch-size 3`; it is identical to the
default-batch export apart from live counters, which is how the cursor paths are
verified.
