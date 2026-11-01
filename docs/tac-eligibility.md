# tac-eligibility

`tac-eligibility` turns the mirrors `clone-repos.zsh` made into the `hashes.js`
that [tac-eligibility-check](https://github.com/LF-Decentralized-Trust/tac-eligibility-check)
serves, where the page md5s a trimmed, lowercased GitHub ID and looks it up in
two lists. The two lists come from different places:

- **nominees**, eligible to run: everyone who authored a commit in the window,
  read from the bare mirrors, with commit addresses resolved to GitHub logins.
- **voters**, eligible to vote: maintainers, meaning everyone a CLOWarden config
  puts in a team holding `write`, `maintain` or `admin` on some repository, plus
  the organization owners — and then only those who could also stand. Holding
  write somewhere is not enough on its own: a maintainer with no commit the
  window counted neither runs nor votes, so the voters are a subset of the
  nominees.

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
| `clones` | `clone-repos.zsh` over both reports into the mirror cache |
| `mirror` | `mirror-clowarden.zsh` into `mirror/`, when `reports/clowarden-watched-repos.csv` is there to refresh from |
| `nominees` | `tac-eligibility --phase authors --phase logins` — the only step that calls the API |
| `voters` | `tac-eligibility --phase voters --phase write` — the roll, then `hashes.js` |

Both lists span both enterprises: every report is passed to the nominee sweep,
and every people export to the voter roll. `REPORTS` and `PEOPLE` at the top of
the script are derived from `ENTERPRISES`, so adding a third enterprise is one
word.

Nothing in the script commits anything, and the steps underneath are what the
rest of this page describes.

Four phases, each resumable and each writing its result into the mirror tree, so a
phase can be re-run on its own:

```sh
./target/release/tac-eligibility --report reports/lf-decentralized-trust.yaml --since 2025-07-01
./target/release/tac-eligibility --phase logins                # resume the API half alone
```

The mirror tree may hold the mirrors of more than one enterprise, so `--report` (YAML
or JSON, repeatable) keeps the count to the repositories that report names, and
says how many of them are not cloned yet. Without it every mirror under
`repos/` counts.

### Keeping people off both rolls

`staff.yaml` at the root of this repository — `staff.yml` and `staff.json` are
read too — names logins that neither roll may carry, with a reason each. It is
the one hand-written input to these phases that **is tracked in git**: it holds
public logins and a reason, nothing more, and who was excluded is part of the
record of an election.

The first cut is generated, then edited:

```sh
./target/release/tac-eligibility --seed-staff \
  --people peeps/lf-decentralized-trust.yaml --people peeps/openwallet-foundation.yaml
```

That writes everyone a CLOWarden config *or* GitHub puts on a team whose name
ends in `staff`, `bot` or `bots` — the teams that exist to give the foundation
and its automation the access to administer a repository, which is not the same
as maintaining it. It refuses to overwrite an existing file, and it stops
there: nothing counts the file until the `logins` and `voters` phases run
again. Read it first. A team called `*-bot` sometimes holds the *person* who
looks after the bot, and somebody on `lf-staff` can also be a real maintainer;
deleting their line is how they keep their vote.

Three shapes are accepted, so the file can be written the way it reads best:

```yaml
# login: why -- what --seed-staff writes, and what the reports quote
some-staffer: foundation staff
some-bot:

# or grouped, where the group name becomes the reason
staff:
  - some-staffer
bots:
  - some-bot

# or just the logins
- some-staffer
- some-bot
```

Matching is on the lowercased login. `voters.json` and `logins.json` each list
what the file removed under `staff_dropped`, with the reason, and the phases
print the count — so a roll never shrinks silently.

| Phase | Reads | Writes | Network |
| --- | --- | --- | --- |
| `authors` | the mirrors | `repos/authors.json` | none |
| `logins` | `authors.json`, `repos/cache.json` | `repos/logins.json` | GitHub API |
| `voters` | the CLOWarden configs, the org-members exports, `logins.json`, the mirrors | `repos/voters.json` | none |
| `write` | `logins.json`, `voters.json` | `hashes.js`, `repos/voters.csv` | none |

`authors` reads each mirror with `git log --all --since`, which needs no trees,
so a `--filter=tree:0` mirror answers it offline.

**Spell the window as an instant, not a day.** `--since 2025-10-01` is handed
to git, which reads a bare date as midnight in the machine's own timezone and
prunes its traversal by commit date, so a commit within a few hours of the
boundary can fall either side depending on where the run happens. On the
October 2026 data exactly one commit did, which cost one nominee.
`--since 2025-10-01T00:00:00Z` is unambiguous, and starting a day early avoids
the question altogether.

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
and place that put them there. It then drops every
maintainer the nominee roll does not name, because the vote and the nomination
rest on the same window — so this phase reads `logins.json` and the `logins`
phase has to have run. Who went is in `voters.json` under
`cannot_run_dropped`, and the phase prints both numbers: the maintainers it
found, and the voters left. In the October 2026 run that was 837 maintainers
and 536 voters.

`voters.json` also lists
`orgs_without_people` — organizations the configs declare that no export covers.
Those have no live membership and no owners on the list, which is what a
forgotten `--people` looks like from the outside.

It then records the latest activity in every repository those configs declare
access on, under `repositories`, keyed `org/repo`:

```json
"hyperledger/fabric": {
  "teams": {"fabric-admins": "admin", "fabric-triage": "triage", "security-managers": "read"},
  "collaborators": {},
  "write": true,
  "mirrored": true,
  "latest_commit": "2026-10-28T01:08:34+05:30",
  "commits_in_window": 632
}
```

`write` is true when some team or collaborator holds write, maintain or admin —
only those make voters. `latest_commit` is the newest commit on any ref of the
mirror, whatever the window, and `commits_in_window` counts the window itself:
the two together say whether access that makes voters is access to a repository
anyone still works in. A declared repository with no clone under `--repos` —
archived, private, renamed, never cloned — reads `mirrored: false` and is
counted separately rather than being reported as quiet, and a clone that holds
no refs at all reads as mirrored with `latest_commit: null`. The phase prints
the three numbers:

```
687 repositories with access declared, 661 of them write or better: 94 with no
commit in the window, 41 with no mirror to read
```

Two git traversals per mirror, run `--jobs` at a time, which is seconds over
seven hundred of them — and no network, like the rest of the phase.

`write` renders both arrays into the `hashes.js` of the `tac-eligibility-check`
checkout in this repository and prints how many hashes each list gained and lost
against the file that was there. That one file is all it touches in the
checkout: it runs no git commands, so the checkout's remotes, branch and other
files are left alone, and committing the result is a hand step. The logins
behind the hashes stay in `repos/`, which is gitignored — the published file
holds only digests.

It also writes the voter roll as Helios reads a bulk voter upload, to
`repos/voters.csv` (`--csv` moves it):

```
github,RyJones
github,someone-else
```

Helios takes those fields positionally — `voter_type, voter_id, email, name` —
so the file has **no header row**: the first field of every line has to name an
auth system Helios knows, and a header makes it reject the whole upload with
`invalid voter type 'voter_type'`. LFDT elections authenticate with GitHub, so
`voter_type` is `github` and `voter_id` is the login, which is what Helios'
GitHub auth stores and compares case-insensitively. The `email` and `name`
columns are read only for `voter_type: password`, where Helios makes its own
login and has to mail it, so they are left off rather than filled with a commit
address that reaches nobody. A line with fewer than two fields is skipped in
silence, which is why an empty login never becomes one.

## Publishing the rolls

The phase files are gitignored where they are written, so the record of who was
eligible — and of when that changed — lives in
[ryjones/tac-election-data](https://forgejo.lan.ryjones.dev/ryjones/tac-election-data),
whose push mirror forwards each commit to
github.com/LFDT-way/tac-election-data. The `Publish tac-election-data` workflow
(`.forgejo/workflows/publish-tac-election-data.yaml`) is what writes there:
dispatched by hand with a `since` date, it runs `election.zsh --from people`
and copies `repos/*.json`, `repos/voters.csv` and the two `results/*.yaml`
exports into the root of that repository, one commit per run.

Three things about it are worth knowing before dispatching one:

- **It publishes commit addresses.** `authors.json`, `cache.json` and the
  `unresolved` list in `logins.json` hold the addresses the window turned up.
  The published copy is public; the files are named explicitly in the workflow
  rather than globbed so that stays a decision.
- **`cache.json` travels both ways.** The published copy is seeded back into
  `repos/` before the run, so the `logins` phase pays only for addresses nobody
  has ever looked up. The roll files are deliberately *not* seeded: a failed
  phase must not leave yesterday's roll to be republished as today's.
- **`TAC_DATA_TOKEN`** is a Forgejo token with write access to that
  repository. The automatic per-run token is scoped to this one.

`tac-roll-diff` writes the commit message, and is worth running by hand before
a publish to see what a run would say:

```console
$ tac-roll-diff --before ../tac-election-data --after repos \
      --source github-audit-tools@$(git rev-parse --short HEAD)
Election rolls: 536 voters (+2 / -2), 2270 nominees (+1 / -1)

Window 2025-10-01 to now, from github-audit-tools@61a35d1.

Voters: 536, was 536 (+2 / -2)
  joined -- added maintainer (1)
    0xivanov
  joined -- now active in the window (1)
    0x009922
  left -- no activity (1)
    3100
  left -- no longer a maintainer (1)
    ex-maintainer
```

`--before` is a directory holding the published `voters.json` and
`logins.json`; one that holds neither is a first publish, and the message says
so rather than reporting everyone as having joined. Each mover gets the reason
the *new* files show: `cannot_run_dropped` reads as **no activity**,
`staff_dropped` and `bots_dropped` as the list that now holds them, and absence
from the file altogether as **no longer a maintainer** — the reverse against
the old files for someone who joined.

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
| `--csv FILE` | The Helios bulk voter upload to write (default: `voters.csv` under `--repos`) |
| `--jobs N` / `--api-jobs N` | Mirrors read at a time; API calls at a time |
| `--staff FILE` | The exclusion list; default `staff.yaml`, `staff.yml` or `staff.json` at the repository root |
| `--seed-staff` | Write the first cut of that file from the `*staff` / `*bot` / `*bots` teams, and stop |
