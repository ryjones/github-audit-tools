# tac-eligibility

`tac-eligibility` turns the mirrors `clone-repos.zsh` made into the `hashes.js`
that [tac-eligibility-check](https://github.com/LF-Decentralized-Trust/tac-eligibility-check)
serves, where the page md5s a trimmed, lowercased GitHub ID and looks it up in
two lists. The two lists come from different places:

- **nominees**, eligible to run: everyone who authored a commit in the window,
  read from the bare mirrors, with commit addresses resolved to GitHub logins.
- **voters**, eligible to vote: maintainers, meaning everyone a CLOWarden config
  puts in a team holding `write`, `maintain` or `admin` on some repository, plus
  the organization owners. Neither list is a subset of the other.

It needs three things on disk first: the mirrors (`clone-repos.zsh`), the
CLOWarden configs (`mirror-clowarden.zsh`), and a people export per enterprise
(`gh-org-members`). Only the `logins` phase talks to GitHub, and it resolves a
credential the way every tool here does — `GITHUB_TOKEN`, `GH_TOKEN`, then
`gh auth token`.

## The whole run

`election.zsh` is every step in dependency order, so the usual run is one line:

```sh
./election.zsh                      # window since 2025-10-01
./election.zsh -n                   # print the commands, run nothing
./election.zsh --from nominees      # pick up after the clones
./election.zsh --only voters        # one step
./election.zsh --since 2025-07-01   # a different window
```

| Step | What it runs |
| --- | --- |
| `repos` | `gh-enterprise-repos` for both enterprises into `results/` |
| `people` | `gh-org-members` once per enterprise into `peeps/` |
| `clones` | `clone-repos.zsh` over both reports into `repos/` |
| `mirror` | `mirror-clowarden.zsh` into `mirror/`, when `reports/clowarden-watched-repos.csv` is there to refresh from |
| `nominees` | `tac-eligibility --phase authors --phase logins` — the only step that calls the API |
| `voters` | `tac-eligibility --phase voters --phase write` — the roll, then `hashes.js` |

Both lists span both enterprises: every report is passed to the nominee sweep,
and every people export to the voter roll. `REPORTS` and `PEOPLE` at the top of
the script are derived from `ENTERPRISES`, so adding a third enterprise is one
word.

Nothing in the script commits anything, and the steps underneath are what the
rest of this page describes.

Four phases, each resumable and each writing its result into `repos/`, so a
phase can be re-run on its own:

```sh
./target/release/tac-eligibility --report reports/lf-decentralized-trust.yaml --since 2025-07-01
./target/release/tac-eligibility --phase logins                # resume the API half alone
```

`repos/` may hold the mirrors of more than one enterprise, so `--report` (YAML
or JSON, repeatable) keeps the count to the repositories that report names, and
says how many of them are not cloned yet. Without it every mirror under
`repos/` counts.

| Phase | Reads | Writes | Network |
| --- | --- | --- | --- |
| `authors` | the mirrors | `repos/authors.json` | none |
| `logins` | `authors.json`, `repos/cache.json` | `repos/logins.json` | GitHub API |
| `voters` | the CLOWarden configs, the org-members exports | `repos/voters.json` | none |
| `write` | `logins.json`, `voters.json` | `hashes.js` | none |

`authors` reads each mirror with `git log --all --since`, which needs no trees,
so a `--filter=tree:0` mirror answers it offline.

`logins` asks GitHub who a commit address belongs to, one call per address
(`repos/{org}/{repo}/commits/{sha}` → `author.login`), and keeps the answer in
`repos/cache.json` keyed by address alone. The cache is worth keeping across
enterprises: every address already in it is a call the next run does not make.
An address no account claims is no contribution as GitHub counts it either, so
it is left out and listed in `logins.json` under `unresolved`, busiest first; to
credit one anyway put it in `repos/aliases.json` as `{"email": "login"}`, which
is never looked up and always wins. `--retry-unresolved` asks again about the
ones that came back empty, and `--offline` resolves from the cache alone.

`voters` reads the CLOWarden configs that `mirror-clowarden.zsh` cloned into
`mirror/` for which teams hold write or better, and the export
[gh-org-members](gh-org-members.md) wrote for who is in those teams and who owns
each organization. `voters.json` records which of the three — the config, live
membership, ownership — put each person on the list, so a surprise can be traced
back.

One export covers one enterprise, so `--people` is repeatable and an election
spanning two of them passes both. The exports are merged, not concatenated:
someone both name is one voter, whose `sources` and `where` carry every reason
and place that put them there. `voters.json` also lists
`orgs_without_people` — organizations the configs declare that no export covers.
Those have no live membership and no owners on the list, which is what a
forgotten `--people` looks like from the outside.

`write` renders both arrays into the `hashes.js` of the `tac-eligibility-check`
checkout in this repository and prints how many hashes each list gained and lost
against the file that was there. That one file is all it touches: it runs no git
commands, so the checkout's remotes, branch and other files are left alone, and
committing the result is a hand step. The logins behind the hashes stay in
`repos/`, which is gitignored — the published file holds only digests.

| Flag | Meaning |
| --- | --- |
| `--phase P` | Run only this phase; repeatable |
| `--report FILE` | Count only the repositories this report names; repeatable |
| `--since D` / `--until D` | The activity window (default: since 2025-07-01) |
| `--coauthors` | Credit `Co-authored-by` trailers as activity too |
| `--repos DIR` | The mirrors, and where the intermediate files go |
| `--people FILE` | gh-org-members export; repeatable, one per enterprise, merged (default: `results/lf-decentralized-trust.gh.yaml`) |
| `--clowarden DIR` | The CLOWarden config mirror (default: `mirror/`) |
| `--out FILE` | The `hashes.js` to write (default: `tac-eligibility-check/hashes.js` in this repository) |
| `--jobs N` / `--api-jobs N` | Mirrors read at a time; API calls at a time |
