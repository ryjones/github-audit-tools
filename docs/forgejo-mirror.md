# The Forgejo side of the mirror

`clone-repos.zsh --forgejo` and `update-repos.zsh` fill a Forgejo instance from
the clones under the mirror root. The repositories there are ordinary
repositories, not Forgejo mirrors: each is pushed branch for branch and tag for
tag from the clone on disk and stands on its own afterwards.

This page is about what those runs do to the instance, what they refuse to do,
and the two things that live only on the instance and therefore cannot be fixed
by re-running anything.

## What one repository costs the instance

Per repository, in this order:

1. `GET /api/v1/repos/<org>/<name>`, and `POST /api/v1/orgs/<org>/repos` if it
   is not there yet.
2. `PATCH` to turn the Actions unit off, the first time only (see below).
3. `git ls-remote` over ssh — the divergence check, which also answers
   whether anything is going to move at all.
4. the git-lfs objects — only with `--lfs`, and then only if the clone holds
   pointers **and** the instance's refs are not already the clone's: down from
   github.com, then up to the instance, before the refs that name them.
5. `git push --prune +refs/heads/* +refs/tags/*` over ssh.
6. `PATCH` to set the default branch, when it is not already right.

So two ssh connections per repository, which is why the instance's sshd needs a
`MaxStartups` above the stock `10:30:100` — see
[Dropped connections](#dropped-connections).

## git-lfs is off unless `--lfs` is given

A mirror clone holds the pointer blobs and not one byte of what they stand for,
so mirroring a repository that uses git-lfs means two transfers no other
repository needs: the objects down from github.com, then up to the instance,
before the refs that name them.

Neither is made unless the run is given `--lfs`. Without it the pointers go up
with nothing behind them — which is what the bare clone holds anyway — the push
does not fail, because `fj_push` passes `--no-verify` and git-lfs's pre-push
hook never runs, and a checkout from the instance gets the pointer text where
the file should be. That is the bargain: the mirror is for counting commits and
reading history, not for building anything.

What the two transfers cost is why. Measured on 2026-10-22:

- `git lfs ls-files --all` walks every ref of the repository to find the
  pointers — 3.7s on `hiero-ledger/solo`, minutes on a large monorepo.
- `git lfs push --all` then uploads every object it found, whether the instance
  already has it or not. `solo` holds **one** object and spent **2m38s
  re-sending the same 123 MB** it had sent on the previous run, and
  `LFDT-Lineth/lineth-monorepo` did the same thing 1,466 times over. Forgejo's
  batch endpoint does not spare the client that.
- and what it bought: **96 objects and 4.7 GiB** on the instance belonged to
  mirrored repositories, out of its 135,277 objects and 12 GiB — the rest is
  `ry/*`, where `ry/alprs` alone is 134,285 of them. The clones held **10.4 GiB**
  more, in five of 730, next to 108 MiB of git-lfs's own lock cache spread over
  631 of them.

With `--lfs`, none of it is started when the instance's branches and tags are
already exactly the clone's — the same names at the same objects, nothing left
over on either side — because then there is no object the instance can be
missing. That is nearly every repository on nearly every run. A repository that
really did change still pays the full price, and `FJ_LFS_ALWAYS=1` pays it
regardless, for an instance whose objects are in doubt.

The comparison drops the peeled lines `ls-remote` prints for annotated tags
(`refs/tags/v1^{}` — 82 of besu's 467); they have no counterpart in
`for-each-ref`.

### Taking back the objects an earlier run pushed

`purge-lfs.zsh` deletes them on both sides:

```sh
./purge-lfs.zsh -n              # the whole plan, repository by repository
./purge-lfs.zsh                 # both halves, after one confirmation
./purge-lfs.zsh --local-only    # the clones' lfs/ directories only
```

Its scope is the bare clones under the mirror root, each named by its own
`remote.origin.url`, so a repository that is not a mirror of github.com is
never in it — that is what keeps `ry/alprs` and `ry/manchaster-ryland`, which
are most of the instance's git-lfs, out of reach. `SLUG` arguments narrow it to
an org or an org/name, exactly as in `update-repos.zsh`.

The two halves are not symmetrical, on purpose. A clone's objects can always be
fetched from github.com again, so every clone in scope loses its `lfs/`
directory, including the repositories named in `forgejo-skip.txt`. The
instance's cannot be fetched from anywhere, so a repository named in that file
is left alone there.

On the instance both halves of an object have to go: the `lfs_meta_object` row,
which is what Forgejo serves pointers from, and the file in the object store at
`[lfs] PATH`, laid out as `<oid[0:2]>/<oid[2:4]>/<oid[4:]>`. The store is
content-addressed and shared by every repository on the instance, so a file is
removed only once no row anywhere still names its oid; one another repository
shares is reported as kept. `repository.lfs_size` and `repository.size` are
corrected for the repositories whose rows went. The rows go in one transaction
and the files only after it commits — the other order could leave the instance
serving a pointer to a file that is no longer there.

That work needs the database credentials and the object store, so it is done
inside the container rather than over the API, which has no call for it: `ssh
<host>` then `pct exec <ct>`, reading `PASSWD` out of `/etc/forgejo/app.ini`
in there and never putting it on a command line. `--host` and `--ct` default to
`pve` and `301`, the node and the container the instance runs in.
Nothing is deleted before the plan has been printed and confirmed; `-n` stops
after the plan and `-y` answers in advance, which a run with no terminal needs.

A file in the store that no row has *ever* named is a different problem, and
Forgejo's own `gc_lfs` cron is for those.

## Actions are off on every mirrored repository

A mirror's workflows are github.com's. With the Actions unit on, every branch
and every tag pushed in creates workflow runs on the instance. By 2026-10-22 it
had collected **196,977 runs** — 149,911 of them in one mirrored repository,
178,701 of them failures nobody reads — and **308 `schedule:` triggers**
registered against runners that were never meant to build any of it.

The unit is the only switch Forgejo has; there is no per-workflow one, and
turning it off takes `workflow_dispatch` with it, which a mirror has no use for
either. Both scripts now do it as step 2 above, and say so once:

```
[ 42/778] Briolette/governance cloned, 1.2M in 3s, forgejo repo created, 2 refs updated, actions turned off
…
Done in 2472s: 778 cloned, 0 updated, 778 pushed to https://forgejo.lan.ryjones.dev (12 changed), 777 with actions turned off, 0 failed; 1.2T in /Volumes/graid/github-mirror.
```

Later runs are quiet about it, because the unit is already off. To check one by
hand:

```
curl -s https://forgejo.lan.ryjones.dev/api/v1/repos/<org>/<name> | jq .has_actions
```

To sweep every repository already on the instance without fetching from GitHub
first:

```
./update-repos.zsh --no-fetch
```

A repository named in `forgejo-skip.txt` is never reached, so a copy on the
instance that something else maintains keeps whatever units it has.

## Two things a run cannot undo

Both live in the instance's database, so re-mirroring does not clear them.

### Schedules already registered

`schedule:` triggers are parsed from the **default branch** and stored as rows
in `action_schedule` and `action_schedule_spec`. Turning the unit off does not
delete the rows that are already there. Delete them in the instance's Postgres,
having checked first that only the intended repositories are in scope:

```
select r.owner_name || '/' || r.name, count(*)
  from action_schedule s join repository r on r.id = s.repo_id
 group by 1 order by 2 desc;

delete from action_schedule_spec where repo_id = <id>;
delete from action_schedule      where repo_id = <id>;
```

Run it from inside the container, which has the credentials: read `HOST`,
`NAME`, `USER` and `PASSWD` out of the `[database]` section of
`/etc/forgejo/app.ini` into `PGPASSWORD` and `psql` there, rather than copying
the password anywhere. Then confirm once that a later push has not put the rows
back.

### The runs themselves

Turning the unit off leaves the runs where they are — they are rows, not units.
On 2026-10-22 the 196,810 still there after the sweep were deleted for every
repository whose unit is off, which is all 718 mirrored ones:

```sql
-- scope first, into temp tables, while the parent rows still exist
create temp table off_repo as
  select r.id from repository r
   where not exists (select 1 from repo_unit u where u.repo_id = r.id and u.type = 10);
create temp table del_run  as select id from action_run     where repo_id in (select id from off_repo);
create temp table del_job  as select id from action_run_job where run_id  in (select id from del_run);
create temp table del_task as select id from action_task    where job_id  in (select id from del_job);
-- then child to parent, in one transaction
delete from action_task_output where task_id in (select id from del_task);
delete from action_task_step   where task_id in (select id from del_task);
delete from action_task        where id      in (select id from del_task);
delete from action_run_job     where id      in (select id from del_job);
delete from action_run         where id      in (select id from del_run);
```

196,780 runs, 257,263 jobs, 604 tasks and 53 outputs went. 30 runs survived,
every one of them real CI on a `ry/*` repository — including all 20 of the
instance's successful runs, because nothing in the deleted set had ever
succeeded. `action_artifact` was empty and no deleted task had a log file, so
there was nothing to remove from disk. `action_run_index` (per-repository run
numbering) and `action_tasks_version` are left alone on purpose: counters, not
history.

Scope it with the `repo_unit` test rather than a list of repositories — that is
the same switch `fj_actions_off` sets, so the two cannot disagree. Run it from
inside the container as above. A plain `vacuum (analyze)` afterwards took the
four tables from 1,078 MB to 128 MB; most of what is left is deleted index
pages marked reusable, which a `reindex` would hand back.

## Two things are never pushed over

- a repository named in `forgejo-skip.txt`, whose copy on the instance is the
  maintained one. One `ORG/NAME` per line, case-insensitive. The clone is still
  fetched from github.com; only the push is skipped.
- a repository whose copy on the instance holds a branch or tag pointing at an
  object the clone does not have — work that exists only there, which a forced
  push would throw away.

Both are reported and left alone. A repository whose refs on the instance
**could not be read at all** is reported as a failure instead: that question
has to be answered before a push that forces every branch and prunes
everything else, and a run that cannot answer it leaves the repository for the
next one.

## Dropped connections

The instance is reached over ssh, and sshd turns away whatever is over its
`MaxStartups` before authentication. The stock `10:30:100` starts refusing at
random above ten pre-auth connections, and `-j` defaults to one worker per core
with two connections each, so drops are routine at that setting:

```
sshd[315]: drop connection #12 from [10.73.42.244]:51859 on [10.73.42.80]:22 Maxstartups
```

Two things follow from that:

- the instance wants a `MaxStartups` to match the client, as a drop-in in
  `/etc/ssh/sshd_config.d/` — `60:30:120` for a 24-worker client;
- a push or an `ls-remote` that fails this way is tried again, three attempts in
  all, two seconds apart and then four. The retry is printed while it waits,
  and a repository that needed one says so in its line.

A failure that is reported after that is a real one. The reported message is
ssh's own words, not git's closing advice about access rights, which says the
same thing whether the connection was dropped or the key was wrong.
