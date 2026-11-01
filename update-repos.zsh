#!/usr/bin/env zsh
#
# update-repos.zsh -- bring the clones clone-repos.zsh made up to date with
# github.com, and the Forgejo copies up to date with the clones.
#
#   ./update-repos.zsh                      # every clone in the mirror tree
#   ./update-repos.zsh hyperledger          # one organization
#   ./update-repos.zsh hyperledger/fabric trustoverip
#   ./update-repos.zsh --local-only         # the clones only, leave Forgejo
#   ./update-repos.zsh --no-fetch           # Forgejo only, from what is on disk
#
# It reads no report: what it updates is what is on disk, found by walking
# DIR/<org>/<name> for bare clones. DIR is the same mirror tree clone-repos.zsh
# writes -- /Volumes/graid/github-mirror when that array is mounted, else
# $GITHUB_MIRROR_DIR or repos/ beside the script -- and -d overrides it. A repository that has appeared on GitHub
# since the last run is therefore not here -- that is clone-repos.zsh's job, and
# it can be re-run over the report whenever the inventory is refreshed.
#
# Each clone's own `remote.origin.url` says which GitHub repository it is, so a
# repository stored as _github is pushed to Forgejo under its real name.
#
# The Actions unit is turned off on every repository the mirror creates or
# pushes into, because a mirror's workflows are github.com's: with the unit on,
# every branch and every tag pushed in creates workflow runs here, and the
# instance had collected 196,977 of them by 2026-10-22 -- 149,911 of them in
# one mirrored repository -- along with 308 `schedule:` triggers aimed at
# runners that were never meant to build any of it. Repositories named in
# forgejo-skip.txt are not touched, here as everywhere else. Schedules already
# registered on the instance are rows in its database; see docs/forgejo-mirror.md.
#
# Two things are never pushed over: a repository named in forgejo-skip.txt,
# whose copy on the instance is the one that is maintained, and any repository
# whose copy there holds a commit the clone does not have. Both are reported
# and left alone rather than forced. Neither is a repository whose refs on the
# instance could not be read at all: that question has to be answered before a
# push that forces every branch and prunes everything else, so one it could not
# be answered for is reported as a failure and left for the next run.
#
# git-lfs objects are not mirrored unless --lfs is given. A mirror clone holds
# the pointer blobs and nothing they stand for, so mirroring them means two
# transfers no other repository needs -- down from github.com, then up to the
# instance -- and they are the most expensive thing a run does: 96 objects on
# the instance were 4.7 of its 12 GiB of git-lfs on 2026-10-22, and the clones
# held 10.4 GiB more. Without --lfs the pointers are pushed with nothing behind
# them, which is what a bare mirror clone holds anyway: the push does not fail,
# and a checkout from the instance gets the pointer text in place of the file.
#
# With --lfs the objects go up before the refs that name them, and even then
# none of it is started when the instance's branches and tags are already
# exactly the clone's -- git-lfs uploads every object it finds rather than the
# ones the instance lacks, one of which cost 2m38s and 123 MB of re-upload on
# 2026-10-22. FJ_LFS_ALWAYS=1 does it anyway.
#
# purge-lfs.zsh takes back the objects a run with --lfs left on the instance
# and in the clones.
#
# A push, or the listing of the instance's refs that has to come before it,
# whose ssh connection the instance refused before it was a connection -- sshd
# turns away whatever is over its MaxStartups limit, and one worker per core
# opens two connections per repository -- is tried again, twice, a few seconds
# apart. Both the retry and a push that needed one are reported.
#
# A clone with no branches and no tags is an empty repository upstream: it is
# reported as one and nothing is created on Forgejo for it, which is a fact
# rather than a failure.
#
# A fetch of a large repository or a push of a few hundred tags can take minutes
# with nothing to show, so every --progress seconds (10 by default) each
# repository still working gets a line of its own saying how long it has been at
# it and what it is doing. Only the ones slower than that interval are named.
#
# The Forgejo copies are plain repositories, not Forgejo mirrors: every branch
# and every tag is pushed, and anything deleted upstream is pruned from the
# Forgejo side too. A clone fetched with --filter=tree:0 has no trees or blobs
# to push, so it is refetched whole the first time it is pushed; --local-only
# leaves such a clone exactly as it is.

setopt err_exit no_unset pipe_fail extended_glob
zmodload zsh/datetime

SELF=${0:A}
LIB=${SELF:h}/forgejo-lib.zsh
FJ_LIB=0
if [[ -r $LIB ]]; then
  source $LIB
  FJ_LIB=1
else
  # No library, so no Forgejo and no progress record either. Stub the stage
  # calls rather than wrapping each one in a conditional.
  fj_stage_init() { : }
  fj_stage() { : }
  fj_stage_done() { : }
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

# How many refs a porcelain fetch or push touched, as a phrase.
ref_note() {
  local n=$1 what=$2
  if (( n == 0 )); then
    print -r -- "up to date"
  elif (( n == 1 )); then
    print -r -- "$what 1 ref"
  else
    print -r -- "$what $n refs"
  fi
}

# The line of git's output worth printing when a command fails. The last line
# is usually the least informative one -- `error: failed to push some refs to`
# after git-lfs has already said which object it could not find -- so the
# hints, the blank lines and git-lfs's standing advice about lock verification
# are dropped and the last line that remains is the one that said something.
#
# An ssh connection that never happened is the exception: ssh says why, and git
# then prints four lines of standing advice about access rights and the
# repository existing, the last of which -- `and the repository exists.` -- is
# what the rule below would keep. So ssh's own line wins when there is one.
why_failed() {
  local dropped= line= chatter=
  if (( FJ_LIB )); then
    dropped=$(fj_transport_error $1)
    if [[ -n $dropped ]]; then
      print -r -- $dropped
      return 0
    fi
  fi
  # git's closing lines say nothing the run did not already know: `error:
  # failed to push some refs to` comes after whatever the real complaint was,
  # and the three about access rights and the repository existing come after an
  # ssh failure whatever its cause. They go with the hints, the blank lines and
  # git-lfs's standing advice about lock verification.
  chatter='^[[:space:]]*$|^hint:|^Locking support detected|^[[:space:]]*\$ git config'
  chatter+='|^error: failed to push some refs to|^fatal: Could not read from remote repository'
  chatter+='|^Please make sure you have the correct access rights|^and the repository exists'
  line=$(grep -vE $chatter $1 | tail -1 || :)
  # Nothing but chatter, which is still better said than not said at all.
  if [[ -z $line ]]; then
    line=$(grep -vE '^[[:space:]]*$' $1 | tail -1 || :)
  fi
  print -r -- $line
}

# True when a clone holds neither a branch nor a tag: the repository upstream
# has no commits yet. Nothing to push, and nothing wrong.
is_empty() {
  [[ -z $(git -C $1 for-each-ref --count=1 --format=x refs/heads refs/tags 2>/dev/null) ]]
}

# One repository, run by xargs in parallel. Reached only through the private
# --update-one flag, which carries the shared settings in UR_* variables.
update_one() {
  local idx=$1 dir=$2 org=$3 name=$4
  local label="[${(l:${#UR_TOTAL}:)idx}/$UR_TOTAL] $org/$name"
  local started=$EPOCHREALTIME
  local errors=$(mktemp) porcelain=$(mktemp) rc=0 note= why=

  if [[ -n ${UR_INFLIGHT:-} ]]; then
    fj_stage_init $UR_INFLIGHT/$idx "$org/$name"
  fi

  if (( UR_FETCH )); then
    fj_stage "fetching from github.com"
    git -C $dir fetch --porcelain --prune > $porcelain 2> $errors || rc=$?
    if (( rc )); then
      why=$(why_failed $errors)
      print -ru2 -- "$label FAILED (fetch, git exit $rc): ${why:-no output}"
      printf '%s\t%s\t%s\t%s\n' "$org/$name" fetch $rc "${why:-no output}" >> $UR_FAILURES
      fj_stage_done
      rm -f -- $errors $porcelain
      return 0
    fi
    note=$(ref_note $(grep -c . $porcelain || :) fetched)
    print -r -- fetched >> $UR_TALLY
  else
    note="not fetched"
  fi
  rm -f -- $porcelain

  # No commits upstream yet. Say so plainly, count it, and leave Forgejo out of
  # it: a repository there would hold nothing, and this is not a failure.
  if is_empty $dir; then
    print -r -- "$label $note, empty repository: no branches or tags to push"
    print -r -- empty >> $UR_TALLY
    fj_stage_done
    rm -f -- $errors
    return 0
  fi

  if (( UR_FORGEJO )); then
    # No report here, so no description to set and no default branch to prefer:
    # the clone's own HEAD says which branch GitHub calls the default.
    if ! fj_sync $dir $org $name '' '' $errors; then
      why=$(why_failed $errors)
      print -ru2 -- "$label FORGEJO FAILED ($FJ_ERROR): ${why:-no further output}"
      printf '%s\t%s\t%s\t%s\n' "$org/$name" forgejo 1 "$FJ_ERROR: ${why:-no further output}" >> $UR_FAILURES
      fj_stage_done
      rm -f -- $errors
      return 0
    fi
    note+=$FJ_NOTE
    if (( FJ_PUSHED )); then
      print -r -- pushed >> $UR_TALLY
    fi
    if (( FJ_CHANGED )); then
      print -r -- changed >> $UR_TALLY
    fi
    if (( FJ_SKIPPED )); then
      print -r -- skipped >> $UR_TALLY
    fi
    if (( FJ_LFS )); then
      print -r -- lfs >> $UR_TALLY
    fi
    if (( FJ_ACTIONS )); then
      print -r -- actions >> $UR_TALLY
    fi
  fi

  local took=$(printf '%.0fs' $(( EPOCHREALTIME - started )))
  print -r -- "$label $note, $(du -sh $dir | cut -f1 | tr -d ' \t') in $took"
  fj_stage_done
  rm -f -- $errors
  return 0
}

if [[ ${1:-} == --update-one ]]; then
  shift
  (( $# == 4 )) || die "--update-one takes INDEX DIR ORG NAME" 2
  if (( ${UR_FORGEJO:-0} )); then
    (( FJ_LIB )) || die "$LIB is missing" 127
    fj_require || die "$FJ_ERROR" 127
  fi
  update_one "$@"
  exit 0
fi

ncpu=$(sysctl -n hw.ncpu 2> /dev/null || print 8)
forgejo_default=${FORGEJO_URL:-https://forgejo.lan.ryjones.dev}

# Where the mirrors live. They are a cache of github.com -- every one of them
# can be fetched again -- so they belong on the big array rather than inside
# this repository, and keeping one tree across runs is what makes an update a
# fetch instead of a clone. -d wins, then $GITHUB_MIRROR_DIR, then the array if
# it is mounted, and repos/ beside the script when it is not.
MIRROR_DEFAULT=${GITHUB_MIRROR_DEFAULT:-/Volumes/graid/github-mirror}

default_root() {
  if [[ -n ${GITHUB_MIRROR_DIR:-} ]]; then
    print -r -- $GITHUB_MIRROR_DIR
  elif [[ -d ${MIRROR_DEFAULT:h} ]]; then
    print -r -- $MIRROR_DEFAULT
  else
    print -r -- ${SELF:h}/repos
  fi
}

usage() {
  print -r -- "usage: ${SELF:t} [-d DIR] [-j JOBS] [-n] [--local-only] [--no-fetch]
       ${${SELF:t}//?/ } [--lfs] [--forgejo-url URL] [--progress SECS] [SLUG...]

  -d DIR           the clone tree to update (default: $(default_root))
  -j JOBS          repositories at a time (default: $ncpu)
  -n               list what would be updated and stop
  --local-only     fetch the clones from github.com and stop; no Forgejo
  --no-fetch       push to Forgejo from the clones as they are; no fetch
  --forgejo-url U  the instance (default: \$FORGEJO_URL, else
                   $forgejo_default)
  --lfs            mirror git-lfs objects too -- fetch them from github.com
                   and upload them to the instance, before the refs that name
                   them. Off by default: the pointers are pushed with nothing
                   behind them, which is what the bare clone holds
  --no-lfs         the default, said out loud; it wins if both are given
  --progress SECS  how often to name the repositories that are still working
                   (default: 10; 0 says nothing until each one finishes)
  SLUG             limit to these repositories, each an org or an org/name;
                   the default is every clone found under DIR"
}

zparseopts -D -E -F -- d:=opt_dir j:=opt_jobs n=opt_dry \
  -local-only=opt_local -no-fetch=opt_nofetch -forgejo-url:=opt_fj_url \
  -lfs=opt_lfs -no-lfs=opt_no_lfs -progress:=opt_progress \
  h=opt_help -help=opt_help || { usage >&2; exit 2 }
if (( $#opt_help )); then
  usage
  exit 0
fi
[[ ${1:-} == -- ]] && shift

for tool in git jq xargs; do
  (( $+commands[$tool] )) || die "$tool is required and was not found" 127
done

root=${opt_dir[2]:-$(default_root)}
jobs=${opt_jobs[2]:-$ncpu}
# zparseopts hands back --forgejo-url=URL with the = still on the front.
fj_url=${${opt_fj_url[2]:-}#=}
# zparseopts hands back --progress=SECS with the = still on the front.
progress=${${opt_progress[2]:-10}#=}
[[ $progress == <0-> ]] || die "--progress takes seconds, not ${(q)progress}"
# Both halves of it -- the workers' stage records and the watcher -- live in
# the library, so without the library there is no progress to report.
(( FJ_LIB )) || progress=0
[[ $jobs == <1-> ]] || die "-j takes a positive number, not ${(q)jobs}"
[[ -d $root ]] || die "$root is not a directory; clone-repos.zsh makes it"

# git-lfs, the one thing a run does not do unless it is asked: see the top of
# the file for what it costs and what leaving it out means. --no-lfs is the
# default said out loud, so it wins if both are given.
lfs=0
(( $#opt_lfs )) && lfs=1
(( $#opt_no_lfs )) && lfs=0
export FJ_LFS_ENABLED=$lfs

fetch=1
forgejo=1
(( $#opt_nofetch )) && fetch=0
(( $#opt_local )) && forgejo=0
if (( ! fetch && ! forgejo )); then
  die "--local-only and --no-fetch together leave nothing to do"
fi
if (( forgejo )); then
  (( FJ_LIB )) || die "$LIB is missing or unreadable; --local-only works without it" 127
  (( $+commands[curl] )) || die "talking to Forgejo needs curl, which was not found" 127
fi

# Every bare clone two levels down, as TAB-separated dir, org and name. The
# org and name come from the clone's own origin URL, so the _github-for-.github
# spelling on disk does not reach Forgejo; a clone with no usable origin falls
# back to its path, where a leading underscore is read as a dot again.
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
(( $#found )) || die "no bare clones under $root; run clone-repos.zsh first"

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

total=$#selected
typeset -a orgs=(${(u)${(@)${(@)selected#*$'\t'}%%$'\t'*}})

if (( $#opt_dry )); then
  plan="fetched from github.com"
  if (( ! fetch )); then
    plan="read as they are"
  fi
  if (( forgejo )); then
    plan+=" and pushed to ${fj_url:-$forgejo_default}"
    if (( lfs )); then
      plan+=", git-lfs objects and all"
    else
      plan+=", git-lfs pointers without their objects"
    fi
  fi
  print -r -- "$total of $#found clones under $root would be $plan, in ${#orgs} organization(s):"
  idx=0
  for row in $selected; do
    idx=$(( idx + 1 ))
    dir=${row%%$'\t'*}
    org=${${row#*$'\t'}%%$'\t'*}
    name=${row##*$'\t'}
    partial=''
    if (( forgejo )) && fj_is_partial $dir; then
      partial=$'\t''(commits only, would be refetched whole)'
    fi
    printf '%s\t%s/%s\t%s%s\n' $idx $org $name ${dir#"$root"/} $partial
  done
  exit 0
fi

tmp=$(mktemp -d)
# The curl config holding the Forgejo token lives in here, so take the
# interrupt and the hangup as seriously as the normal exit.
watcher=0
cleanup() {
  # The heartbeat is a background subshell, and a run killed with its terminal
  # cannot kill it from the normal path; without this it sleeps on for ever.
  if (( watcher )); then
    kill $watcher 2> /dev/null || :
    watcher=0
  fi
  rm -rf -- $tmp
}
trap cleanup EXIT
trap 'cleanup; exit 130' INT TERM HUP

export UR_FAILURES=$tmp/failures
export UR_TALLY=$tmp/tally
: > $UR_FAILURES
: > $UR_TALLY

export UR_FETCH=$fetch
export UR_FORGEJO=$forgejo
export UR_TOTAL=$total
export GIT_TERMINAL_PROMPT=0
export GIT_SSH_COMMAND=${GIT_SSH_COMMAND:-"ssh -o BatchMode=yes"}

# The organizations, once, here rather than in the workers: several workers
# creating the same organization at the same moment is a race worth not having.
if (( forgejo )); then
  fj_init $tmp/forgejo.curlrc $fj_url || die "$FJ_ERROR"
  # Repositories whose copy on the instance is the maintained one; they are
  # fetched from github.com like any other and never pushed. See the file.
  if fj_skip_init ${FORGEJO_SKIP_FILE:-${SELF:h}/forgejo-skip.txt}; then
    print -r -- "Leaving the $(fj_skip_count) repositories named in ${FJ_SKIP_FILE:t} alone."
  fi
  # What --lfs is doing or not doing, said once: either way the pointers go
  # up, and without the objects a checkout from the instance gets the pointer
  # text where the file should be.
  if (( lfs )); then
    if fj_lfs_available; then
      print -r -- "Mirroring git-lfs objects as well (--lfs)."
    else
      print -ru2 -- "Warning: --lfs was given but git-lfs is not installed. A repository that uses it will push its pointers with no objects behind them."
    fi
  else
    print -r -- "Not mirroring git-lfs objects: a repository that uses it pushes its pointers only (--lfs mirrors them)."
  fi
  print -r -- "Forgejo $FJ_URL: ${#orgs} organization(s), pushing over ssh as $FJ_SSH_USER."
  typeset -a failed_orgs=()
  for org in $orgs; do
    if fj_ensure_org $org; then
      print -r -- "  $org: organization $FJ_ACTION"
    else
      print -ru2 -- "  $org: organization FAILED: $FJ_ERROR"
      printf '%s\t%s\t%s\t%s\n' "$org" organization 1 "$FJ_ERROR" >> $UR_FAILURES
      failed_orgs+=($org)
    fi
  done
  if (( $#failed_orgs )); then
    if (( $#failed_orgs == $#orgs )); then
      die "no organization could be created on $FJ_URL: $FJ_ERROR"
    fi
    # Nothing can be pushed into an organization that is not there.
    typeset -a kept=()
    for row in $selected; do
      org=${${row#*$'\t'}%%$'\t'*}
      if (( ! ${failed_orgs[(Ie)$org]} )); then
        kept+=("$row")
      fi
    done
    selected=($kept)
    total=$#selected
    export UR_TOTAL=$total
    print -ru2 -- "  skipping the ${#failed_orgs} organization(s) above; $total repositories left"
    (( total )) || die "nothing left to update"
  fi
fi

# NUL-separated, so no path or name could ever split into two arguments.
idx=0
for row in $selected; do
  idx=$(( idx + 1 ))
  printf '%s\0%s\0%s\0%s\0' $idx ${row%%$'\t'*} ${${row#*$'\t'}%%$'\t'*} ${row##*$'\t'} >> $tmp/queue
done

what=()
(( fetch )) && what+=("fetching from github.com")
(( forgejo )) && what+=("pushing every branch and tag to $FJ_URL")
print -r -- "Updating $total clones under $root, $jobs at a time: ${(j: and :)what}."
started=$EPOCHREALTIME

# Each worker keeps a file saying what it is doing; the watcher prints the ones
# that have been doing it a while, so a slow fetch or a push of several hundred
# tags is visible while it happens rather than only once it is over.
if (( progress )); then
  mkdir -p $tmp/inflight
  export UR_INFLIGHT=$tmp/inflight
  # trap -, because a background subshell inherits this shell's traps: a signal
  # to the process group would otherwise have the heartbeat run cleanup and
  # delete the run's temporary directory from under it. Its own two checks and
  # the kill below are what stop it.
  { unsetopt err_exit no_unset pipe_fail
    trap - EXIT INT TERM HUP
    fj_progress_watch $tmp/inflight $progress 120 $$ } &
  watcher=$!
fi

xargs -0 -n 4 -P $jobs $SELF --update-one < $tmp/queue || :

if (( watcher )); then
  kill $watcher 2> /dev/null || :
  wait $watcher 2> /dev/null || :
  watcher=0
fi

# A worker that died outright left its stage record behind, because nothing ran
# fj_stage_done. Without this the repository has no line anywhere -- not
# cloned, not failed, not counted -- which is how hyperledger-indy/indy-vdr
# vanished from a run on 2026-10-22. There is nothing to check when --progress
# kept no records.
if [[ -n ${UR_INFLIGHT:-} ]]; then
  for orphan in ${UR_INFLIGHT}/*(N.); do
    IFS=$'\t' read -r orphan_start orphan_slug orphan_stage orphan_pid < $orphan 2>/dev/null || continue
    print -ru2 -- "${orphan_slug:-${orphan:t}} FAILED (the worker stopped without finishing, at ${orphan_stage:-an unknown stage})"
    printf '%s\t%s\t%s\t%s\n' "${orphan_slug:-${orphan:t}}" worker 1 \
      "the worker stopped without finishing, at ${orphan_stage:-an unknown stage}" >> $UR_FAILURES
  done
fi

fetched=$(grep -c '^fetched$' $UR_TALLY || :)
pushed=$(grep -c '^pushed$' $UR_TALLY || :)
changed=$(grep -c '^changed$' $UR_TALLY || :)
empty=$(grep -c '^empty$' $UR_TALLY || :)
skipped=$(grep -c '^skipped$' $UR_TALLY || :)
lfs=$(grep -c '^lfs$' $UR_TALLY || :)
actions=$(grep -c '^actions$' $UR_TALLY || :)
failed=$(grep -c . $UR_FAILURES || :)
summary=()
(( fetch )) && summary+=("$fetched fetched")
(( forgejo )) && summary+=("$pushed pushed to $FJ_URL ($changed changed)")
(( empty )) && summary+=("$empty empty")
(( skipped )) && summary+=("$skipped left alone")
(( lfs )) && summary+=("$lfs with git-lfs objects")
(( actions )) && summary+=("$actions with actions turned off")
summary+=("$failed failed")
print -r -- "Done in $(printf '%.0fs' $(( EPOCHREALTIME - started ))): ${(j:, :)summary}; $(du -sh $root | cut -f1 | tr -d ' \t') in $root."

if (( failed )); then
  print -ru2 -- ""
  print -ru2 -- "Failed:"
  sort $UR_FAILURES | while IFS=$'\t' read -r slug action rc why; do
    if [[ $action == (forgejo|organization|worker) ]]; then
      print -ru2 -- "  $slug ($action): $why"
    else
      print -ru2 -- "  $slug ($action, git exit $rc): $why"
    fi
  done
  exit 1
fi
