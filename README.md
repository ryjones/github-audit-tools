# github-audit-tools

Tools for auditing a GitHub enterprise: what repositories it holds, who can
reach them, which advisories are open against them, and what Actions storage
they are paying for. Four Rust binaries and nine scripts, each answering a
question that came up often enough to be worth automating.

Everything here reads GitHub rather than changing it, with one exception
(`gh-actions-storage purge`, which deletes only with `--apply`). What
`clone-repos.zsh --forgejo` and `update-repos.zsh` write, they write to a
Forgejo instance of your own; GitHub is still only read.

## The tools

| Tool | Answers | Docs |
| --- | --- | --- |
| `gh-enterprise-repos` | Every repository in an enterprise or organization, by visibility and archived state, as YAML — optionally with the teams that reach each one | [docs](docs/gh-enterprise-repos.md) |
| `gh-org-members` | Every person in an enterprise, the organizations they belong to, and the teams they sit on | [docs](docs/gh-org-members.md) |
| `gh-org-reports` | The gaps in a people export: organization members on no team, enterprise members in no organization | [docs](docs/gh-org-members.md#reports) |
| `gh-actions-storage` | Which repositories hold the most live Actions artifact storage, and purging the old ones | [docs](docs/gh-actions-storage.md) |
| `clowarden-drift` | Where an organization's declared access (its CLOWarden config) and its real access disagree, one record per disagreement | [docs](docs/gh-org-members.md#reconciling-against-github) |
| `tac-eligibility` | Who may vote and who may stand in the LFDT TAC election, as the `hashes.js` the eligibility page serves and the voter roll Helios uploads | [docs](docs/tac-eligibility.md) |
| `tac-roll-diff` | How the election rolls moved between two runs, and why, as the commit message for a publish of the data | [docs](docs/tac-eligibility.md#publishing-the-rolls) |
| `gh-enterprise-ghsa.zsh` | Every security advisory across an enterprise's organizations, with CVEs, and which repositories have private vulnerability reporting off | [docs](docs/gh-enterprise-ghsa.md) |

And the scripts that build on their output:

| Script | What it does |
| --- | --- |
| `election.zsh` | Runs the whole TAC election pipeline in order — both exports, the clones, the configs, then `hashes.js` |
| `clone-repos.zsh` | Bare-mirrors every public, non-archived repository a `gh-enterprise-repos` report — or `--org LOGIN` — names into the mirror cache as `<org>/<name>`, with `--visibility` for the private and internal ones too, and with `--forgejo` creates each org and repo on a Forgejo instance, turns that repo's Actions unit off, and pushes every branch and tag into it; `--lfs` mirrors git-lfs objects too, which it does not do by default |
| `update-repos.zsh` | Fetches those clones from GitHub again and pushes the changes on to Forgejo, leaving alone anything named in `forgejo-skip.txt` or holding commits only the instance has; `--no-fetch` does the Forgejo side alone, and `--lfs` mirrors git-lfs objects too |
| `purge-lfs.zsh` | Deletes the git-lfs objects an earlier `--lfs` run left behind — the clones' `lfs/` directories, and the instance's `lfs_meta_object` rows and object-store files — after printing the whole plan and asking |
| `drift.zsh` | Compares one organization's export with the CLOWarden `config.yaml` meant to describe it |
| `find-clowarden-configs.zsh` | Finds which repository holds each organization's CLOWarden config, and names the organizations that have none |
| `mirror-clowarden.zsh` | Clones, or fetches, every CLOWarden config repository into `mirror/<org>/<repo>` |
| `clowarden-history.py` | Derives team membership history from the git history of those configs — years, where the audit log serves about 180 days |
| `fetch-audit-history.zsh` | Pulls each login's org and team membership history out of the enterprise audit log |

## Build

```sh
make                 # all seven binaries, debug, into target/debug
make release         # optimized, into target/release
make check           # fmt, clippy, the tests, and a syntax pass over the scripts
./target/release/gh-org-members --help
```

`make help` lists the targets; `cargo build [--release]` does the same job if
you would rather.

The scripts need no build, only `zsh`, [`gh`](https://cli.github.com), `jq`,
`yq` and `python3`; `clowarden-history.py` uses the standard library and `yq`.
`git-lfs` as well, but only for `--lfs`, which neither mirror script does by
default: without that flag a repository using git-lfs pushes its pointers with
no objects behind them, and both scripts say which of the two they are doing at
the start of the run rather than leaving you to find out.

## Authentication

Every binary and script resolves a credential the same way: `GITHUB_TOKEN`,
then `GH_TOKEN`, then `gh auth token`. Each one prints which it used.

One organization can need a credential of its own. An organization that has
turned classic personal access tokens off answers 403 to everything — itself,
its repositories, a file in one of them — so its requests go out with the token
in `GITHUB_<LOGIN>_TOKEN` (`GITHUB_LFDT_LINETH_TOKEN` for `LFDT-Lineth`), and
it is named in `org-tokens.txt` because an enterprise listing returns a null
where it should be and never its login. See
[an organization with its own credential](docs/gh-org-members.md#an-organization-with-its-own-credential).

Prefer `gh`'s own credential. A classic PAT has to be SSO-authorized for each
organization separately, and an organization it was not authorized for is not
an error — `enterprise.organizations` simply does not return it, and its people
and repositories are then missing from the export with nothing to say so. That
trap is the subject of [whose view an export is](docs/gh-org-members.md#whose-view-an-export-is),
and it is worth reading before trusting a number from any of these tools.

Scopes, by what you are asking for: `read:org` throughout,
`read:enterprise` for anything enterprise-wide, `read:audit_log` for the audit
history, organization owner or security manager for advisories, and write
access to Actions to purge artifacts.

```sh
env -u GITHUB_TOKEN -u GH_TOKEN ./target/release/gh-org-members -e acme-inc -o results/people.yaml
```

## Working end to end

Two workflows chain most of the above:

- **Who really has access** —
  [docs/clowarden-reconciliation.md](docs/clowarden-reconciliation.md) walks an
  enterprise from a people export through the CLOWarden configs to a drift
  report, and records the gotchas each step has.
- **LFDT TAC election eligibility** —
  [docs/tac-eligibility.md](docs/tac-eligibility.md): export the repositories,
  mirror them, count commit authors in the window, take the maintainers who
  could also stand, write `hashes.js` and the Helios voter upload.
  `./election.zsh` is that whole run, one step at a time, and
  `./election.zsh -n` prints the commands without running any of them.

## Where output goes

Three hand-written files are tracked on purpose: `staff.yaml`, the logins kept
off both TAC election rolls, with a reason each; `forgejo-skip.txt`, the
Forgejo repositories no run may push into because the copy there is the
maintained one; and `org-tokens.txt`, the organizations that need a credential
of their own. All three say something about the world rather than about this
machine, and none of them holds a secret.

The bare mirrors are a cache of github.com and live outside the repository:
`/Volumes/graid/github-mirror/<org>/<name>` when that array is mounted, else
`$GITHUB_MIRROR_DIR`, else `repos/` beside the scripts. github.com is cloned
into that cache, and Forgejo is fed from it — github.com → cache → Forgejo. A
repository using git-lfs keeps its objects in the cache too, under the clone's
own `lfs/`, so a second `--lfs` run has them already — and `purge-lfs.zsh`
removes them again, on both sides, when they are not wanted.

Five directories hold what the tools write, and they divide in two.

`results/` and `reports/` are **tracked**: the repository exports and the
reports built from them. A default export is public, non-archived repositories
only, so it says nothing github.com does not already say — and having it in
git means a diff shows what moved in the enterprise between two runs, which is
most of why the exports are worth keeping.

That holds only for a default run. `--visibility all` writes the names of
private and internal repositories, and a published copy of this tree has
existed before; an export made that way is not one to commit. The `source:`
header of an export also names the credential it was made with and that
credential's scopes.

`peeps/`, `repos/` and `mirror/` are gitignored: people exports, bare clones
when the cache falls back here, and config checkouts. The first names real
people and the other two are not ours to carry. Keep it that way — every
example in the docs is invented for that reason, which is also why the
advisory samples are made up rather than pasted.

## Layout

```
Makefile              make, make release, make check
Cargo.toml            one package, seven binaries
src/
  client.rs           the GraphQL client gh-enterprise-repos and
                      gh-org-members share
  rest_client.rs      the REST client gh-actions-storage and
                      tac-eligibility share
  repos_*.rs          repository inventory
  members_*.rs        people, teams, audit sweeps, reports
  storage_*.rs        Actions artifact storage
  clowarden*.rs       the CLOWarden configs, and the drift against them
  token.rs            which credential a request uses, for the one
                      organization that refuses classic tokens
  tac_*.rs            the TAC election phases
  bin/<tool>.rs       one file per binary
docs/                 one page per tool, plus the reconciliation HOWTO and
                      the Forgejo-mirror page
*.zsh, *.py           the scripts, at the root
forgejo-lib.zsh       sourced, not run: the Forgejo API calls, the push, the
                      refetch, the git-lfs transfers --lfs asks for, the two
                      don't-push-over-it guards and the progress reporting
                      that clone-repos.zsh and update-repos.zsh share
github-lib.zsh        sourced, not run: which GitHub credential a request
                      uses, the same rule src/token.rs applies to the binaries
forgejo-skip.txt      Forgejo repositories no run may push into
org-tokens.txt        organizations that need a credential of their own
```

`src` is flat, so each module carries the prefix of the tool that owns it;
three tools had a `client.rs` and two had a `model.rs`.

## Provenance

Each tool grew up in its own repository — `gh-enterprise-repos`,
`gh-org-members`, `gh-enterprise-ghsa`, `gh-actions-storage`. They were folded
in here with `git subtree`, so every commit and every blame line survives the
move. The archived repositories keep their own copies of that history.

`tac-eligibility-check/` is a checkout of
[the LFDT election page](https://github.com/LF-Decentralized-Trust/tac-eligibility-check),
not part of this repository and gitignored. `tac-eligibility` writes its
`hashes.js` and nothing else — no git commands, so its remotes and branch are
left alone. Review and commit it there yourself.

## License

Apache License 2.0 — see [LICENSE](LICENSE).
