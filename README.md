# github-audit-tools

Tools for auditing a GitHub enterprise: what repositories it holds, who can
reach them, which advisories are open against them, and what Actions storage
they are paying for. Four Rust binaries and eight scripts, each answering a
question that came up often enough to be worth automating.

Everything here reads GitHub rather than changing it, with one exception
(`gh-actions-storage purge`, which deletes only with `--apply`).

## The tools

| Tool | Answers | Docs |
| --- | --- | --- |
| `gh-enterprise-repos` | Every repository in an enterprise or organization, by visibility and archived state, as YAML — optionally with the teams that reach each one | [docs](docs/gh-enterprise-repos.md) |
| `gh-org-members` | Every person in an enterprise, the organizations they belong to, and the teams they sit on | [docs](docs/gh-org-members.md) |
| `gh-org-reports` | The gaps in a people export: organization members on no team, enterprise members in no organization | [docs](docs/gh-org-members.md#reports) |
| `gh-actions-storage` | Which repositories hold the most live Actions artifact storage, and purging the old ones | [docs](docs/gh-actions-storage.md) |
| `clowarden-drift` | Where an organization's declared access (its CLOWarden config) and its real access disagree, one record per disagreement | [docs](docs/gh-org-members.md#reconciling-against-github) |
| `tac-eligibility` | Who may vote and who may stand in the LFDT TAC election, as the `hashes.js` the eligibility page serves | [docs](docs/tac-eligibility.md) |
| `gh-enterprise-ghsa.zsh` | Every security advisory across an enterprise's organizations, with CVEs, and which repositories have private vulnerability reporting off | [docs](docs/gh-enterprise-ghsa.md) |

And the scripts that build on their output:

| Script | What it does |
| --- | --- |
| `election.zsh` | Runs the whole TAC election pipeline in order — both exports, the clones, the configs, then `hashes.js` |
| `clone-repos.zsh` | Bare-mirrors every public, non-archived repository a `gh-enterprise-repos` report names into `repos/<org>/<name>` |
| `drift.zsh` | Compares one organization's export with the CLOWarden `config.yaml` meant to describe it |
| `find-clowarden-configs.zsh` | Finds which repository holds each organization's CLOWarden config, and names the organizations that have none |
| `mirror-clowarden.zsh` | Clones, or fetches, every CLOWarden config repository into `mirror/<org>/<repo>` |
| `clowarden-history.py` | Derives team membership history from the git history of those configs — years, where the audit log serves about 180 days |
| `fetch-audit-history.zsh` | Pulls each login's org and team membership history out of the enterprise audit log |

## Build

```sh
make                 # all six binaries, debug, into target/debug
make release         # optimized, into target/release
make check           # fmt, clippy, the tests, and a syntax pass over the scripts
./target/release/gh-org-members --help
```

`make help` lists the targets; `cargo build [--release]` does the same job if
you would rather.

The scripts need no build, only `zsh`, [`gh`](https://cli.github.com), `jq`,
`yq` and `python3`; `clowarden-history.py` uses the standard library and `yq`.

## Authentication

Every binary and script resolves a credential the same way: `GITHUB_TOKEN`,
then `GH_TOKEN`, then `gh auth token`. Each one prints which it used.

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
  mirror them, count commit authors in the window, union the maintainers, write
  `hashes.js`. `./election.zsh` is that whole run, one step at a time, and
  `./election.zsh -n` prints the commands without running any of them.

## Where output goes

`results/`, `reports/`, `peeps/`, `repos/` and `mirror/` hold what the tools
write: repository exports, reports, people exports, bare clones and config
checkouts. All five are gitignored,
because they name real people and real private repositories. Keep it that way —
every example in the docs is invented for that reason.

## Layout

```
Makefile              make, make release, make check
Cargo.toml            one package, six binaries
src/
  client.rs           the GraphQL client gh-enterprise-repos and
                      gh-org-members share
  rest_client.rs      the REST client gh-actions-storage and
                      tac-eligibility share
  repos_*.rs          repository inventory
  members_*.rs        people, teams, audit sweeps, reports
  storage_*.rs        Actions artifact storage
  clowarden*.rs       the CLOWarden configs, and the drift against them
  tac_*.rs            the TAC election phases
  bin/<tool>.rs       one file per binary
docs/                 one page per tool, plus the reconciliation HOWTO
*.zsh, *.py           the scripts, at the root
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
