#!/usr/bin/env zsh
#
# purge-lfs.zsh -- delete the git-lfs objects a mirror run put somewhere: the
# ones in the clones under the mirror root, and the ones on the Forgejo
# instance.
#
#   ./purge-lfs.zsh -n                      # what would go, and nothing else
#   ./purge-lfs.zsh                         # both halves, after one confirmation
#   ./purge-lfs.zsh --local-only            # the clones' lfs/ directories only
#   ./purge-lfs.zsh --forgejo-only hiero-ledger
#
# clone-repos.zsh and update-repos.zsh no longer mirror git-lfs objects unless
# they are given --lfs. This takes back the ones the runs that came before left
# behind: on 2026-10-22 the clones held 10.4 GiB of them, and of the instance's
# 135,277 objects and 12 GiB, 96 objects and 4.7 GiB belonged to mirrored
# repositories. The rest is Ry's own -- ry/alprs alone is 134,285 objects --
# and nothing here can reach it. See below.
#
# Scope, which is the whole safety story: the repositories are the bare clones
# found under the mirror root, DIR/<org>/<name>, the same walk update-repos.zsh
# does, and each one's org and name come from its own remote.origin.url. So a
# repository that is not a clone of github.com under that root is not in scope
# at any stage -- the ry/* repositories on the instance are nobody's mirror,
# are not in the tree, and are therefore never named in the delete. SLUG
# arguments narrow it further, each an org or an org/name.
#
# The two halves differ in one thing, deliberately:
#
#   * the clones' objects can always be fetched from github.com again, so
#     every clone in scope has its lfs/ directory removed, including the
#     repositories named in forgejo-skip.txt, whose clones are ours like any
#     other;
#   * the instance's objects cannot be fetched from anywhere, so a repository
#     named in forgejo-skip.txt is left alone there -- the copy on the
#     instance is the one that is maintained, and its objects may be the only
#     ones there are.
#
# On the instance both halves of an object have to go: the row in
# lfs_meta_object, which is what Forgejo serves pointers from, and the file in
# the local object store. The store is content-addressed and shared by every
# repository on the instance, so a file is removed only once no row anywhere
# still names its oid; one that another repository shares is reported as kept.
# repository.lfs_size and repository.size are corrected for the repositories
# whose rows went, so the instance's own numbers stay true.
#
# That work is done inside the Forgejo container rather than over the API,
# which has no call for it: an ssh to the node, then `pct exec` into the
# container, where the [database] credentials and the object store both are.
# The password is read from app.ini in there and never leaves it. --host and
# --ct say where to go, and default to pve and 301.
#
# Nothing is deleted before the whole plan has been printed -- every repository
# with its object count and size, on both sides -- and confirmed. -n prints the
# plan and stops; -y answers the question in advance, which a run with no
# terminal needs.
#
# What this does not touch: a file in the store that no row has ever named
# (Forgejo's own gc_lfs cron is for those), the pointers in the mirrored
# history, which are ordinary git blobs and stay, and anything at all belonging
# to a repository outside the scope above.

setopt err_exit no_unset pipe_fail extended_glob
zmodload zsh/datetime

SELF=${0:A}
LIB=${SELF:h}/forgejo-lib.zsh
FJ_LIB=0
if [[ -r $LIB ]]; then
  source $LIB
  FJ_LIB=1
fi

die() {
  print -ru2 -- "${SELF:t}: $1"
  exit ${2:-1}
}

# org/name as the remote URL spells it: scheme, credentials and host off,
# `.git` off. Handles both https://github.com/org/name and git@github.com:org/name.
slug_of_url() {
  local url=${1%/}
  url=${url%.git}
  url=${url#*://}
  url=${url#*@}
  url=${url#*[:/]}
  print -r -- $url
}

# A byte count as something a person can read, in the units du prints.
human() {
  local -F size=$1
  local -a units=(B K M G T P)
  local i=1
  while (( size >= 1024 && i < $#units )); do
    size=$(( size / 1024.0 ))
    (( i++ ))
  done
  if (( i == 1 )); then
    printf '%d%s' $size $units[i]
  else
    printf '%.1f%s' $size $units[i]
  fi
}

# "1 object" / "2 objects", because the counts here are often one. A third
# argument is the plural, for the words an s does not make plural.
plural() {
  if (( $1 == 1 )); then
    print -r -- "$1 $2"
  else
    print -r -- "$1 ${3:-${2}s}"
  fi
}

# The mirror root is clone-repos.zsh's decision, so ask it rather than copy the
# rule; -d overrides, and repos/ beside the script is the answer if that script
# has gone missing.
default_root() {
  local asked=
  if [[ -x ${SELF:h}/clone-repos.zsh ]]; then
    asked=$(${SELF:h}/clone-repos.zsh --print-root 2>/dev/null || :)
  fi
  print -r -- ${asked:-${SELF:h}/repos}
}

usage() {
  print -r -- "usage: ${SELF:t} [-d DIR] [-n] [-y] [--local-only] [--forgejo-only]
       ${${SELF:t}//?/ } [--host HOST] [--ct ID] [SLUG...]

  -d DIR           the clone tree to purge (default: $(default_root))
  -n               print the plan and stop; delete nothing
  -y              do not ask before deleting; a run with no terminal needs it
  --local-only     the lfs/ directories in the clones only; leave the instance
  --forgejo-only   the instance only; leave the clones as they are
  --host HOST      the node to ssh to for the Forgejo container
                   (default: \$FORGEJO_SSH_HOST, else pve)
  --ct ID          the container the instance runs in
                   (default: \$FORGEJO_CT, else 301)
  SLUG             limit to these repositories, each an org or an org/name;
                   the default is every clone found under DIR"
}

zparseopts -D -E -F -- d:=opt_dir n=opt_dry y=opt_yes \
  -local-only=opt_local -forgejo-only=opt_fj_only \
  -host:=opt_host -ct:=opt_ct \
  h=opt_help -help=opt_help || { usage >&2; exit 2 }
if (( $#opt_help )); then
  usage
  exit 0
fi
[[ ${1:-} == -- ]] && shift

root=${opt_dir[2]:-$(default_root)}
# zparseopts hands back --host=VALUE with the = still on the front.
ssh_host=${${opt_host[2]:-${FORGEJO_SSH_HOST:-pve}}#=}
ct=${${opt_ct[2]:-${FORGEJO_CT:-301}}#=}
[[ $ct == <1-> ]] || die "--ct takes a container id, not ${(q)ct}"

# A tab, named, because several of the patterns below are nothing but one.
TAB=$'\t'

do_local=1
do_forgejo=1
(( $#opt_local )) && do_forgejo=0
(( $#opt_fj_only )) && do_local=0
if (( ! do_local && ! do_forgejo )); then
  die "--local-only and --forgejo-only together leave nothing to do"
fi

[[ -d $root ]] || die "$root is not a directory; clone-repos.zsh makes it"
if (( do_forgejo )); then
  (( $+commands[ssh] )) || die "the Forgejo half needs ssh, which was not found" 127
  # Without the library there is no forgejo-skip.txt, and the one thing that
  # must not happen here is deleting the objects of a repository whose copy on
  # the instance is the maintained one. --local-only works without it.
  (( FJ_LIB )) || die "the Forgejo half needs $LIB, which is missing or unreadable" 127
fi

# Every bare clone two levels down, as TAB-separated dir, org and name, exactly
# as update-repos.zsh finds them: the clone's own origin URL says which GitHub
# repository it is, so a clone stored as _github is named .github on the
# instance.
typeset -a found=()
for dir in $root/*/*(N/); do
  [[ $dir == *.partial ]] && continue
  [[ -d $dir/objects ]] || continue
  url=$(git -C $dir config --get remote.origin.url 2>/dev/null || :)
  slug=
  if [[ -n $url ]]; then
    slug=$(slug_of_url $url)
  fi
  if [[ $slug != [^/]##/[^/]## ]]; then
    slug=${dir:h:t}/${${dir:t}/#_/.}
  fi
  found+=("$dir"$'\t'"${slug%%/*}"$'\t'"${slug#*/}")
done
(( $#found )) || die "no bare clones under $root; there is nothing to purge"

# The arguments, if any, name organizations or org/name pairs. Both the real
# name and the name on disk match, since either is a reasonable thing to type.
typeset -a selected=()
if (( $# )); then
  typeset -A wanted=()
  for arg in $@; do
    wanted[${(L)${arg%/}}]=1
  done
  for row in $found; do
    dir=${row%%$'\t'*}
    org=${${row#*$'\t'}%%$'\t'*}
    name=${row##*$'\t'}
    if (( ${+wanted[${(L)org}]} + ${+wanted[${(L)org}/${(L)name}]} + ${+wanted[${(L)org}/${(L)${dir:t}}]} )); then
      selected+=("$row")
    fi
  done
  (( $#selected )) || die "none of ${(j:, :)${(q)@}} matched a clone under $root"
else
  selected=($found)
fi

typeset -a orgs=(${(u)${(@)${(@)selected#*$'\t'}%%$'\t'*}})
print -r -- "${#selected} of ${#found} clones under $root are in scope, in ${#orgs} organization(s)."

tmp=$(mktemp -d)
cleanup() { rm -rf -- $tmp }
trap cleanup EXIT
trap 'cleanup; exit 130' INT TERM HUP

# ---------------------------------------------------------------- the clones --
#
# What a clone's lfs/ directory holds is two different things. The objects are
# under lfs/objects, and only a handful of clones have any: five of the 730 on
# 2026-10-22, holding 10.4 GiB between them. Every other lfs/ directory is
# git-lfs's own bookkeeping -- a cache/locks/refs/... file per ref, which it
# writes whenever it is asked anything at all, 108 MiB of them across 631 clones
# -- next to an empty objects/ or tmp/. The whole directory goes either way,
# but only the clones with objects in them are worth a line of their own.
typeset -a with_objects=()
typeset -a cache_only=()
local_bytes=0
local_files=0
cache_bytes=0
if (( do_local )); then
  typeset -a lfs_dirs=()
  typeset -A slug_of=()
  for row in $selected; do
    dir=${row%%$'\t'*}
    [[ -d $dir/lfs ]] || continue
    lfs_dirs+=($dir)
    slug_of[$dir]=${${row#*$'\t'}%%$'\t'*}/${row##*$'\t'}
  done
  if (( $#lfs_dirs )); then
    print -r -- "Measuring the lfs/ directory of $(plural $#lfs_dirs clone) …"
    # One du for all of them: the array is one path per clone, and 730 du calls
    # of their own on a spinning array cost a minute.
    printf '%s\0' ${^lfs_dirs}/lfs | xargs -0 du -sk > $tmp/du.tsv
    typeset -A lfs_kb=()
    # Not `read -r kb path`: $path is tied to $PATH in zsh, and reading into it
    # leaves the script without find, wc or rm.
    while IFS=$'\t' read -r kb measured; do
      lfs_kb[${measured%/lfs}]=$kb
    done < $tmp/du.tsv
    for dir in $lfs_dirs; do
      files=0
      if [[ -d $dir/lfs/objects ]]; then
        files=$(find $dir/lfs/objects -type f | wc -l | tr -d ' ')
      fi
      bytes=$(( ${lfs_kb[$dir]:-0} * 1024 ))
      if (( files )); then
        with_objects+=("$dir"$'\t'"$slug_of[$dir]"$'\t'"$bytes"$'\t'"$files")
        local_bytes=$(( local_bytes + bytes ))
        local_files=$(( local_files + files ))
      else
        cache_only+=("$dir")
        cache_bytes=$(( cache_bytes + bytes ))
      fi
    done
  fi
fi

# --------------------------------------------------------------- the instance --
#
# One shell script, built here and run by `sh -s` inside the container. It is
# fed over ssh's stdin rather than its command line, so the list of
# repositories can be any length, and it is the same script in both modes: plan
# counts and deletes nothing, delete does the work and reports what went.
typeset -a fj_scope=()
fj_left_out=0
if (( do_forgejo )); then
  # Said out loud either way: a missing skip list is not a list with nothing
  # on it, and the difference here is whether a maintained repository's objects
  # are about to be deleted.
  if fj_skip_init ${FORGEJO_SKIP_FILE:-${SELF:h}/forgejo-skip.txt}; then
    print -r -- "Leaving the $(fj_skip_count) repositories named in ${FJ_SKIP_FILE:t} alone on the instance."
  else
    print -ru2 -- "Warning: no readable ${${FORGEJO_SKIP_FILE:-forgejo-skip.txt}:t}, so nothing is held back from the instance."
  fi
  for row in $selected; do
    org=${${row#*$'\t'}%%$'\t'*}
    name=${row##*$'\t'}
    # Never on the instance: its copy is the maintained one, and its objects
    # may be the only ones there are.
    if fj_skipped $org $name; then
      fj_left_out=$(( fj_left_out + 1 ))
      continue
    fi
    fj_scope+=("$org/$name")
  done
fi

# remote_payload MODE FILE -- the script to run in the container.
remote_payload() {
  local mode=$1 out=$2
  {
    print -r -- "MODE=$mode"
    print -r -- "INI=${FORGEJO_APP_INI:-/etc/forgejo/app.ini}"
    cat <<'REMOTE_HEAD'
set -eu
umask 077
tab=$(printf '\t')

fail() { printf 'ERROR%s%s\n' "$tab" "$1"; exit 3; }

[ -r "$INI" ] || fail "$INI is not readable in the container"
command -v psql > /dev/null 2>&1 || fail "psql is not installed in the container"

# One [section] of app.ini, and one KEY out of a section's text. The value is
# taken verbatim after the = so a password with spaces in it survives; it is
# only ever put into PGPASSWORD, never onto a command line.
section() { sed -n "/^\[$1\]/,/^\[/p" "$INI"; }
field() { printf '%s\n' "$2" | sed -n "s/^[[:space:]]*$1[[:space:]]*=[[:space:]]*//p" | head -1; }

db=$(section database)
hostport=$(field HOST "$db")
dbname=$(field NAME "$db")
dbuser=$(field USER "$db")
sslmode=$(field SSL_MODE "$db")
PGPASSWORD=$(field PASSWD "$db")
export PGPASSWORD
if [ -z "$hostport" ] || [ -z "$dbname" ] || [ -z "$dbuser" ]; then
  fail "the [database] section of $INI does not name HOST, NAME and USER"
fi
dbhost=${hostport%%:*}
dbport=${hostport##*:}
if [ "$dbport" = "$hostport" ]; then dbport=5432; fi
# sslrootcert=system because the instance is on verify-full and root in the
# container has no ~/.postgresql/root.crt; Go found the same roots by itself.
CONN="host=$dbhost port=$dbport dbname=$dbname user=$dbuser sslmode=${sslmode:-prefer} sslrootcert=system"

store=$(section lfs)
alt=$(section storage.lfs)
storage=$(field STORAGE_TYPE "$store")
lfspath=$(field PATH "$store")
if [ -z "$storage" ]; then storage=$(field STORAGE_TYPE "$alt"); fi
if [ -z "$lfspath" ]; then lfspath=$(field PATH "$alt"); fi
if [ -z "$lfspath" ]; then lfspath=/var/lib/forgejo/data/lfs; fi
case "${storage:-local}" in
  local) ;;
  *) fail "the object store is STORAGE_TYPE $storage, not local; only a local store is handled here" ;;
esac
[ -d "$lfspath" ] || fail "$lfspath is not a directory in the container"
printf 'STORE%s%s\n' "$tab" "$lfspath"

list=$(mktemp)
orphans=$(mktemp)
trap 'rm -f "$list" "$orphans"' EXIT HUP INT TERM

REMOTE_HEAD
    print -r -- "cat > \"\$list\" <<'SLUGS'"
    if (( $#fj_scope )); then
      print -rl -- $fj_scope
    fi
    print -r -- 'SLUGS'
    cat <<'REMOTE_TAIL'

psql_at() { psql -q -At -F "$tab" -v ON_ERROR_STOP=1 "$CONN"; }

# The scope is matched by owner/name rather than by id, case-insensitively,
# because that is how the clone on disk names it. A repository in the list that
# the instance does not have simply does not join.
if [ "$MODE" = plan ]; then
  psql_at <<SQL
create temp table want (slug text);
\copy want (slug) from '$list'
create temp table scope as
  select r.id, r.owner_name || '/' || r.name as slug
    from repository r join want w
      on lower(r.owner_name || '/' || r.name) = lower(btrim(w.slug));
select 'MATCHED', count(*) from scope;
select 'REPO', s.slug, count(m.id), coalesce(sum(m.size), 0)
  from scope s join lfs_meta_object m on m.repository_id = s.id
 group by s.slug order by 4 desc, 2;
select 'ROWS', count(*), coalesce(sum(size), 0)
  from lfs_meta_object where repository_id in (select id from scope);
select 'KEPT', count(distinct m.oid)
  from lfs_meta_object m
 where m.repository_id in (select id from scope)
   and exists (select 1 from lfs_meta_object o
                where o.oid = m.oid
                  and o.repository_id not in (select id from scope));
SQL
  exit 0
fi

# The rows first, in one transaction, and the files only once it has committed:
# a file deleted beside a row that then rolled back would leave the instance
# serving a pointer to nothing. The other way round leaves a file nobody
# references, which is the harmless half of the bargain and is reported.
psql_at <<SQL
begin;
create temp table want (slug text);
\copy want (slug) from '$list'
create temp table scope as
  select r.id, r.owner_name || '/' || r.name as slug
    from repository r join want w
      on lower(r.owner_name || '/' || r.name) = lower(btrim(w.slug));
select 'MATCHED', count(*) from scope;
select 'REPO', s.slug, count(m.id), coalesce(sum(m.size), 0)
  from scope s join lfs_meta_object m on m.repository_id = s.id
 group by s.slug order by 4 desc, 2;
create temp table doomed (oid text, size bigint);
with d as (
  delete from lfs_meta_object
   where repository_id in (select id from scope)
  returning oid, size
) insert into doomed select oid, size from d;
select 'ROWS', count(*), coalesce(sum(size), 0) from doomed;
select 'LOCKS', count(*) from lfs_lock where repo_id in (select id from scope);
delete from lfs_lock where repo_id in (select id from scope);
update repository r set lfs_size = 0, size = r.git_size
 where r.id in (select id from scope) and r.lfs_size <> 0;
select 'KEPT', count(*) from (
  select distinct d.oid from doomed d
   where exists (select 1 from lfs_meta_object m where m.oid = d.oid)) k;
\copy (select distinct d.oid from doomed d where not exists (select 1 from lfs_meta_object m where m.oid = d.oid)) to '$orphans'
commit;
SQL

# Every oid nothing names any more, as the store spells it: the first two
# characters, the next two, then the rest. Anything that is not a hex oid is
# counted and left alone rather than turned into a path.
removed=0
freed=0
absent=0
refused=0
while IFS= read -r oid; do
  [ -n "$oid" ] || continue
  case "$oid" in
    *[!0-9a-f]*) refused=$((refused + 1)); continue ;;
  esac
  if [ ${#oid} -lt 5 ]; then refused=$((refused + 1)); continue; fi
  head=${oid%"${oid#??}"}
  rest=${oid#??}
  mid=${rest%"${rest#??}"}
  file="$lfspath/$head/$mid/${rest#??}"
  if [ -f "$file" ]; then
    size=$(stat -c %s "$file" 2>/dev/null || echo 0)
    if rm -f "$file"; then
      removed=$((removed + 1))
      freed=$((freed + size))
      rmdir "$lfspath/$head/$mid" 2>/dev/null || :
      rmdir "$lfspath/$head" 2>/dev/null || :
    else
      refused=$((refused + 1))
    fi
  else
    absent=$((absent + 1))
  fi
done < "$orphans"
printf 'GONE%s%s%s%s\n' "$tab" "$removed" "$tab" "$freed"
printf 'ABSENT%s%s\n' "$tab" "$absent"
printf 'REFUSED%s%s\n' "$tab" "$refused"
REMOTE_TAIL
  } > $out
}

# run_remote MODE -- the payload for MODE, run in the container, with its
# tagged lines left in $tmp/MODE.out. Non-zero only when ssh or the script
# itself failed; an ERROR line is the script saying why.
run_remote() {
  local mode=$1 rc=0
  remote_payload $mode $tmp/$mode.sh
  ssh -o BatchMode=yes $ssh_host "pct exec $ct -- /bin/sh -s" \
    < $tmp/$mode.sh > $tmp/$mode.out 2> $tmp/$mode.err || rc=$?
  local complaint=$(grep -m1 "^ERROR$TAB" $tmp/$mode.out 2>/dev/null || :)
  if [[ -n $complaint ]]; then
    die "the Forgejo container refused: ${complaint#ERROR$TAB}"
  fi
  if (( rc )); then
    print -ru2 -- "${SELF:t}: ssh $ssh_host 'pct exec $ct' exited $rc"
    [[ -s $tmp/$mode.err ]] && tail -5 $tmp/$mode.err >&2
    return $rc
  fi
  return 0
}

# tagged FILE TAG -- the fields of every line with that tag, tabs and all.
tagged() {
  grep "^$2$TAB" $1 2>/dev/null || :
}

# field N LINE -- one tab-separated field of a tagged line.
field() {
  print -r -- ${${(ps:$TAB:)2}[$1]}
}

typeset -a fj_plan=()
fj_matched=0
fj_rows=0
fj_bytes=0
fj_kept=0
if (( do_forgejo )); then
  if (( ! $#fj_scope )); then
    print -r -- "No repository in scope is pushed to the instance; nothing to ask it."
    do_forgejo=0
  else
    print -r -- "Asking the Forgejo container (ct $ct on $ssh_host) what it holds for ${#fj_scope} repositories …"
    run_remote plan || die "could not read the instance's git-lfs objects"
    fj_store=$(field 2 "$(tagged $tmp/plan.out STORE)")
    fj_matched=$(( $(field 2 "$(tagged $tmp/plan.out MATCHED)") ))
    fj_rows=$(( $(field 2 "$(tagged $tmp/plan.out ROWS)") ))
    fj_bytes=$(( $(field 3 "$(tagged $tmp/plan.out ROWS)") ))
    fj_kept=$(( $(field 2 "$(tagged $tmp/plan.out KEPT)") ))
    plan_rows=$(tagged $tmp/plan.out REPO)
    [[ -z $plan_rows ]] || fj_plan=(${(f)plan_rows})
  fi
fi

# ------------------------------------------------------------------- the plan --
print -r -- ""
if (( do_local )); then
  print -r -- "In the clones under $root:"
  for row in $with_objects; do
    printf '  %-56s %8s in %s\n' "$(field 2 $row)" \
      $(human $(field 3 $row)) "$(plural $(field 4 $row) object)"
  done
  if (( $#with_objects )); then
    print -r -- "  $(plural $#with_objects repository repositories) with git-lfs objects, $(human $local_bytes) in $(plural $local_files object) to delete."
  else
    print -r -- "  nothing: no clone in scope holds a git-lfs object."
  fi
  if (( $#cache_only )); then
    print -r -- "  $(plural $#cache_only clone) more hold nothing but git-lfs's lock cache, $(human $cache_bytes); that goes with it."
  fi
  print -r -- ""
fi
if (( do_forgejo )); then
  print -r -- "On the instance, in ${fj_store:-its object store} and lfs_meta_object:"
  for row in $fj_plan; do
    printf '  %-56s %8s in %s\n' "$(field 2 $row)" \
      $(human $(field 4 $row)) "$(plural $(field 3 $row) object)"
  done
  if (( fj_rows )); then
    print -r -- "  $(plural $fj_rows object), $(human $fj_bytes), in $#fj_plan of the $(plural $fj_matched repository repositories) the instance has in scope."
  else
    print -r -- "  nothing: the instance holds no git-lfs object for any of the ${#fj_scope} repositories in scope."
  fi
  if (( fj_kept )); then
    print -r -- "  $(plural $fj_kept object) of them is also referenced by a repository outside the scope; its file stays."
  fi
  if (( fj_left_out )); then
    print -r -- "  $(plural $fj_left_out repository repositories) in scope were left out, named in ${FJ_SKIP_FILE:t}."
  fi
  print -r -- ""
fi

if (( $#opt_dry )); then
  print -r -- "-n was given; nothing was deleted."
  exit 0
fi
if (( ! $#with_objects && ! $#cache_only && ! fj_rows )); then
  print -r -- "Nothing to delete."
  exit 0
fi

# Hard to undo on the instance and a refetch from github.com on disk, so it is
# asked once, after the plan above and never before it.
if (( ! $#opt_yes )); then
  [[ -t 0 ]] || die "this is not a terminal and -y was not given; nothing was deleted"
  print -n -- "Delete all of that? [y/N] "
  answer=
  read -r answer || :
  print -r -- ""
  [[ ${(L)answer} == (y|yes) ]] || die "not confirmed; nothing was deleted"
fi

started=$EPOCHREALTIME

# ------------------------------------------------------------------- doing it --
freed_local=0
if (( do_local )); then
  print -r -- "Deleting the clones' git-lfs objects …"
  for row in $with_objects; do
    dir=$(field 1 $row)
    slug=$(field 2 $row)
    bytes=$(field 3 $row)
    if rm -rf -- $dir/lfs; then
      freed_local=$(( freed_local + bytes ))
      print -r -- "  $slug: $(human $bytes) removed"
    else
      print -ru2 -- "  $slug: FAILED to remove $dir/lfs"
    fi
  done
  removed_caches=0
  for dir in $cache_only; do
    if rm -rf -- $dir/lfs; then
      removed_caches=$(( removed_caches + 1 ))
    else
      print -ru2 -- "  $slug_of[$dir]: FAILED to remove $dir/lfs"
    fi
  done
  if (( removed_caches )); then
    freed_local=$(( freed_local + cache_bytes ))
    print -r -- "  $(plural $removed_caches 'lock cache' 'lock caches') removed, $(human $cache_bytes)."
  fi
fi

freed_fj=0
if (( do_forgejo && fj_rows )); then
  print -r -- "Deleting the instance's git-lfs objects …"
  run_remote delete || die "the delete on the instance failed; nothing is reported as gone"
  rows=$(( $(field 2 "$(tagged $tmp/delete.out ROWS)") ))
  rows_bytes=$(( $(field 3 "$(tagged $tmp/delete.out ROWS)") ))
  locks=$(( $(field 2 "$(tagged $tmp/delete.out LOCKS)") ))
  kept=$(( $(field 2 "$(tagged $tmp/delete.out KEPT)") ))
  gone=$(( $(field 2 "$(tagged $tmp/delete.out GONE)") ))
  freed_fj=$(( $(field 3 "$(tagged $tmp/delete.out GONE)") ))
  absent=$(( $(field 2 "$(tagged $tmp/delete.out ABSENT)") ))
  refused=$(( $(field 2 "$(tagged $tmp/delete.out REFUSED)") ))
  done_rows=$(tagged $tmp/delete.out REPO)
  if [[ -n $done_rows ]]; then
    for row in ${(f)done_rows}; do
      print -r -- "  $(field 2 $row): $(plural $(field 3 $row) row) deleted, $(human $(field 4 $row))"
    done
  fi
  print -r -- "  $rows lfs_meta_object row(s) ($(human $rows_bytes)), $gone file(s) removed, $(human $freed_fj) freed."
  (( locks )) && print -r -- "  $locks lfs_lock row(s) deleted."
  (( kept )) && print -r -- "  $kept object(s) kept on disk, still referenced outside the scope."
  (( absent )) && print -r -- "  $absent object(s) had no file in the store already."
  (( refused )) && print -ru2 -- "  $refused object(s) could not be removed from the store; they are now unreferenced files."
fi

print -r -- "Done in $(printf '%.0fs' $(( EPOCHREALTIME - started ))): $(human $freed_local) freed in $root, $(human $freed_fj) freed on the instance."
