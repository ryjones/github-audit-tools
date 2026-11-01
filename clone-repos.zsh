#!/usr/bin/env zsh
#
# clone-repos.zsh -- bare-clone every public, non-archived repository named in a
# gh-enterprise-repos report into repos/<org>/<name>, over https, and
# optionally give a Forgejo instance a copy of each one.
#
#   ./clone-repos.zsh reports/lf-decentralized-trust.yaml
#   ./clone-repos.zsh reports/*.yaml            # several enterprises at once
#   ./clone-repos.zsh --forgejo reports/*.yaml  # and push them to Forgejo
#   ./clone-repos.zsh --forgejo --org LFDT-Panurus   # one org, no report first
#
# The report can be YAML or JSON; it goes through yq either way.
#
# Public repositories by default. --visibility private, internal or all takes
# the rest of what the report names, which only a report made with the matching
# gh-enterprise-repos --visibility holds in the first place. Those are cloned
# over ssh with the key github.com already knows, because github.com will not
# serve them over https without a credential and nothing here will put a token
# in a URL. Archived repositories are left out whatever is asked for, and so
# are GitHub's per-advisory private forks: those hold an unpublished fix to
# someone else's advisory and are nobody's repository to mirror. How many were
# left out is reported.
#
# --org LOGIN needs no report at all: it runs gh-enterprise-repos for that
# organization first, into a report inside the run's temporary directory, and
# then goes on as if that report had been given. Repeat it for several
# organizations, and mix it with reports named on the command line if you like.
# Nothing is kept: `gh-enterprise-repos --org LOGIN -d reports` is still the way
# to keep the listing itself.
#
# The clones are a cache of github.com, kept in <root>/<org>/<name> as bare
# mirrors so that a second run is a fetch rather than a clone. The root is
# /Volumes/graid/github-mirror when that array is mounted, $GITHUB_MIRROR_DIR
# if it is set, and repos/ beside the script otherwise; -d overrides all three.
#
# They are kept for counting activity per account over a time period, so by default they are fetched with --filter=tree:0: every commit,
# with its author, committer and dates, and no file content. Pass --full when
# something needs the trees or blobs.
#
# --forgejo creates the organization and the repository on the instance (default
# https://forgejo.lan.ryjones.dev) and pushes every branch and every tag into
# it. These are not Forgejo mirrors: a pull mirror would have the instance fetch
# from github.com on a schedule and keep the repository read-only. Each one here
# is an ordinary repository, filled from the clone on disk, which stands on its
# own afterwards. --forgejo therefore implies --full -- a commits-only clone has
# no trees or blobs to push -- and an existing filtered clone is refetched whole
# before its first push. See forgejo-lib.zsh for the credentials it uses.
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
# A repository with no commits yet is reported as the empty repository it is:
# there is nothing to clone and nothing to push, which is a fact rather than a
# failure, and no repository is created on Forgejo for it.
#
# A clone of a large repository, a refetch, or a push of a few hundred tags can
# take minutes with nothing to show, so every --progress seconds (10 by
# default) each repository still working gets a line of its own saying how long
# it has been at it and what it is doing. Only the ones slower than that
# interval are named, so a run of small repositories prints nothing extra.
#
# Re-running fetches into the clones that are already there, so an interrupted
# run resumes. A repository named .github is cloned as _github, matching how
# dotted repositories are kept elsewhere in the mirror; a dotted directory
# hides itself from globs and from most tooling. On Forgejo it keeps its real
# name, dot and all.

setopt err_exit no_unset pipe_fail
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

# Where a repository lands: its own name, with a leading dot turned into an
# underscore so the clone is not a hidden directory.
on_disk() {
  print -r -- ${1/#./_}
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
# --clone-one flag, which carries the shared settings in CR_* variables.
clone_one() {
  local idx=$1 org=$2 name=$3 url=$4
  local dest=$CR_ROOT/$org/$(on_disk $name)
  local label="[${(l:${#CR_TOTAL}:)idx}/$CR_TOTAL] $org/$name"
  local started=$EPOCHREALTIME
  local -a filter=(${=CR_FILTER})
  local action rc=0
  local errors=$(mktemp)
  local branch= desc=

  if [[ -n ${CR_INFLIGHT:-} ]]; then
    fj_stage_init $CR_INFLIGHT/$idx "$org/$name"
  fi

  # The default branch and the description come from the sidecar rather than
  # from an argument, because BSD xargs -0 drops an empty argument instead of
  # passing it, and either of those two is often empty -- which silently shifts
  # every later field one place left.
  if (( ${CR_FORGEJO:-0} )); then
    local -a meta=("${(@f)$(awk -F'\t' -v i=$idx '$1 == i { print $2; print $3 }' $CR_META)}")
    branch=${meta[1]:-}
    desc=${meta[2]:-}
  fi

  mkdir -p ${dest:h}
  if [[ -d $dest/objects ]]; then
    action=updated
    fj_stage "fetching from github.com"
    git -C $dest fetch --prune --quiet 2> $errors || rc=$?
  else
    action=cloned
    fj_stage "cloning from github.com"
    # Clone aside and rename, so an interrupted run leaves no half-clone that
    # the next run would mistake for a finished one.
    rm -rf -- $dest.partial
    { git clone --mirror --quiet $filter -- $url $dest.partial 2> $errors \
        && mv -- $dest.partial $dest } || rc=$?
  fi

  if (( rc )); then
    local why=$(why_failed $errors)
    print -ru2 -- "$label FAILED ($action, git exit $rc): ${why:-no output}"
    printf '%s\t%s\t%s\t%s\n' "$org/$name" $action $rc "${why:-no output}" >> $CR_FAILURES
    rm -rf -- $dest.partial
    fj_stage_done
    rm -f -- $errors
    return 0
  fi

  # No commits upstream yet. Say so plainly, count it, and leave Forgejo out of
  # it: a repository there would hold nothing, and this is not a failure.
  if is_empty $dest; then
    print -r -- "$label $action, empty repository: no branches or tags to push"
    print -r -- $action >> $CR_TALLY
    print -r -- empty >> $CR_TALLY
    fj_stage_done
    rm -f -- $errors
    return 0
  fi

  # The clone is good; the Forgejo copy is a second stage with its own failure,
  # so a push that fails does not cost the clone that succeeded.
  local note=
  if (( ${CR_FORGEJO:-0} )); then
    if ! fj_sync $dest $org $name "$branch" "$desc" $errors; then
      local why=$(why_failed $errors)
      print -ru2 -- "$label FORGEJO FAILED ($FJ_ERROR): ${why:-no further output}"
      printf '%s\t%s\t%s\t%s\n' "$org/$name" forgejo 1 "$FJ_ERROR: ${why:-no further output}" >> $CR_FAILURES
      print -r -- $action >> $CR_TALLY
      fj_stage_done
      rm -f -- $errors
      return 0
    fi
    note=$FJ_NOTE
    if (( FJ_PUSHED )); then
      print -r -- pushed >> $CR_TALLY
    fi
    if (( FJ_CHANGED )); then
      print -r -- changed >> $CR_TALLY
    fi
    if (( FJ_SKIPPED )); then
      print -r -- skipped >> $CR_TALLY
    fi
    if (( FJ_LFS )); then
      print -r -- lfs >> $CR_TALLY
    fi
    if (( FJ_ACTIONS )); then
      print -r -- actions >> $CR_TALLY
    fi
  fi

  local took=$(printf '%.0fs' $(( EPOCHREALTIME - started )))
  print -r -- "$label $action, $(du -sh $dest | cut -f1 | tr -d ' \t') in $took$note"
  print -r -- $action >> $CR_TALLY
  fj_stage_done
  rm -f -- $errors
  return 0
}

if [[ ${1:-} == --clone-one ]]; then
  shift
  (( $# == 4 )) || die "--clone-one takes INDEX ORG NAME URL" 2
  if (( ${CR_FORGEJO:-0} )); then
    (( FJ_LIB )) || die "$LIB is missing" 127
    fj_require || die "$FJ_ERROR" 127
  fi
  clone_one "$@"
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
  print -r -- "usage: ${SELF:t} [-d DIR] [-j JOBS] [--full] [--forgejo] [--forgejo-url URL]
       ${${SELF:t}//?/ } [--lfs] [--visibility VALUE] [--org LOGIN] [--progress SECS] [-n]
       ${${SELF:t}//?/ } [REPORT...]
       ${${SELF:t}//?/ } --print-root

  --visibility V   which repositories to take from the report: public
                   (default), private, internal, or all. Anything but public
                   is cloned over ssh, because github.com will not hand it
                   over without a credential
  --org LOGIN      list this organization with gh-enterprise-repos and clone
                   what it names; repeatable, and no REPORT is then needed
  -d DIR           where the clones go (default: $(default_root))
  -j JOBS          clones at a time (default: $ncpu)
  --full           clone trees and blobs too, not just commit history
  --forgejo        also create the orgs and repos on a Forgejo instance and
                   push every branch and tag into them; implies --full
  --forgejo-url U  the instance (default: \$FORGEJO_URL, else
                   $forgejo_default)
  --lfs            mirror git-lfs objects too -- fetch them from github.com
                   and upload them to the instance, before the refs that name
                   them. Off by default: the pointers are pushed with nothing
                   behind them, which is what the bare clone holds
  --no-lfs         the default, said out loud; it wins if both are given
  --progress SECS  how often to name the repositories that are still working
                   (default: 10; 0 says nothing until each one finishes)
  -n               list what would be cloned and stop
  --print-root     print the mirror root a run would use, and stop; this
                   script owns that decision, and election.zsh asks it
  REPORT           gh-enterprise-repos report, YAML or JSON; several are merged,
                   and so are any --org listings"
}

zparseopts -D -E -F -- d:=opt_dir j:=opt_jobs n=opt_dry -full=opt_full \
  -forgejo=opt_forgejo -forgejo-url:=opt_fj_url -org+:=opt_org \
  -lfs=opt_lfs -no-lfs=opt_no_lfs -progress:=opt_progress -print-root=opt_print_root -visibility:=opt_vis \
  h=opt_help -help=opt_help || { usage >&2; exit 2 }
if (( $#opt_help )); then
  usage
  exit 0
fi
[[ ${1:-} == -- ]] && shift
# zparseopts collects a repeated option as `--org LOGIN --org LOGIN ...`, and
# hands back --org=LOGIN with the = still on the front.
orgs=(${${opt_org:#--org}#=})

# Where the mirrors live, asked for on its own by anything else that wants to
# work on the same tree.
root=${opt_dir[2]:-$(default_root)}
if (( $#opt_print_root )); then
  print -r -- $root
  exit 0
fi

(( $# >= 1 || $#orgs )) || { usage >&2; exit 2 }

for tool in git jq yq xargs; do
  (( $+commands[$tool] )) || die "$tool is required and was not found" 127
done

# git-lfs, the one thing a run does not do unless it is asked: see the top of
# the file for what it costs and what leaving it out means. --no-lfs is the
# default said out loud, so it wins if both are given.
lfs=0
(( $#opt_lfs )) && lfs=1
(( $#opt_no_lfs )) && lfs=0
export FJ_LFS_ENABLED=$lfs

forgejo=$#opt_forgejo
if (( forgejo )); then
  (( FJ_LIB )) || die "--forgejo needs $LIB, which is missing or unreadable" 127
  (( $+commands[curl] )) || die "--forgejo needs curl, which was not found" 127
fi

reports=($@)
for report in $reports; do
  [[ -r $report ]] || die "cannot read $report"
done

# Which gh-enterprise-repos to run for --org: the release build, the debug
# build, then whatever is on PATH.
repos_tool=
if (( $#orgs )); then
  for candidate in ${SELF:h}/target/release/gh-enterprise-repos \
                   ${SELF:h}/target/debug/gh-enterprise-repos \
                   ${commands[gh-enterprise-repos]:-}; do
    if [[ -n $candidate && -x $candidate ]]; then
      repos_tool=$candidate
      break
    fi
  done
  [[ -n $repos_tool ]] || die "--org needs gh-enterprise-repos; run \`make release\` first" 127
fi

# zparseopts hands back --forgejo-url=URL with the = still on the front.
fj_url=${${opt_fj_url[2]:-}#=}
jobs=${opt_jobs[2]:-$ncpu}
[[ $jobs == <1-> ]] || die "-j takes a positive number, not ${(q)jobs}"
# zparseopts hands back --progress=SECS with the = still on the front.
progress=${${opt_progress[2]:-10}#=}
[[ $progress == <0-> ]] || die "--progress takes seconds, not ${(q)progress}"
# The same four words gh-enterprise-repos uses, so one is not a translation of
# the other.
visibility=${${opt_vis[2]:-public}#=}
[[ $visibility == (public|private|internal|all) ]] \
  || die "--visibility takes public, private, internal or all, not ${(q)visibility}"

# GitHub's per-advisory private forks, never mirrored. Drafting a security
# advisory makes a temporary private fork named for the advisory --
# <repo>-ghsa-xxxx-xxxx-xxxx -- to write the fix in. It is not a repository
# anyone configures or maintains, it reports fork: false so nothing but the
# name gives it away, and what it holds is an unpublished fix to someone
# else's advisory, which is not ours to copy anywhere: 33 of the 56 private
# repositories in the LFDT enterprise were these on 2026-10-22. How many were
# left out is reported, so this is never a silent drop.
advisory_fork='-ghsa-[0-9a-z]{4}-[0-9a-z]{4}-[0-9a-z]{4}$'
# Both halves of it -- the workers' stage records and the watcher -- live in
# the library, so without the library there is no progress to report.
(( FJ_LIB )) || progress=0

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

# One listing per --org, into this run's temporary directory. Its own filters
# are the ones this script wants anyway: public, not archived, forks included.
if (( $#orgs )); then
  mkdir -p $tmp/listings
  print -r -- "Listing ${(j:, :)orgs} with ${repos_tool#${SELF:h}/} …"
  for org in $orgs; do
    $repos_tool --org $org --visibility $visibility -o $tmp/listings/$org.yaml \
      || die "gh-enterprise-repos could not list $org"
    reports+=($tmp/listings/$org.yaml)
  done
fi

# YAML or JSON, through yq either way, one file per report.
for report in $reports; do
  yq -o=json . $report > $tmp/report.json 2>/dev/null \
    || die "$report is not YAML or JSON"
  jq -e '(.repositories | type == "array") and (.source | type == "object")' \
    $tmp/report.json > /dev/null 2>&1 \
    || die "$report is not a gh-enterprise-repos report"
  cat $tmp/report.json >> $tmp/reports.json
done

# What --visibility asks for, and never anything archived, sorted so the
# numbering is stable between runs. A repository two reports both name is
# cloned once. default_branch and description are carried along for the Forgejo
# side; both may be absent.
#
# The clone URL is decided here rather than in the worker, because it depends
# on the visibility and the worker is not told that: github.com serves a public
# repository to anyone over https, and will not hand over a private or an
# internal one without a credential, so those are cloned over ssh with the key
# already registered there. Nothing then has to carry a token.
jq -s --arg visibility "$visibility" --arg forks "$advisory_fork" '
  [ .[].repositories[]
    | select(.archived != true)
    | select($visibility == "all" or (.visibility | ascii_downcase) == $visibility)
    | select(.name | test($forks; "i") | not)
    | { org, name,
        visibility: (.visibility // "PUBLIC"),
        url: (if (.visibility // "PUBLIC") == "PUBLIC"
              then (.url // "https://github.com/\(.org)/\(.name)")
              else "ssh://git@github.com/\(.org)/\(.name).git"
              end),
        default_branch: (.default_branch // ""),
        description: (.description // "") } ]
  | unique_by([.org, .name])
  | sort_by([.org, .name])
' $tmp/reports.json > $tmp/selected.json || die "could not read repositories from the report"

total=$(jq length $tmp/selected.json)
# What that exclusion cost, counted the way it was applied.
forks_left_out=$(jq -s --arg visibility "$visibility" --arg forks "$advisory_fork" '
  [ .[].repositories[]
    | select(.archived != true)
    | select($visibility == "all" or (.visibility | ascii_downcase) == $visibility)
    | select(.name | test($forks; "i"))
    | "\(.org)/\(.name)" ]
  | unique | length
' $tmp/reports.json) || forks_left_out=0

# What the run is about, in the words the messages use.
if [[ $visibility == all ]]; then
  selection='non-archived repositories of every visibility'
else
  selection="$visibility, non-archived repositories"
fi
if (( forks_left_out )); then
  selection+=" ($forks_left_out advisory private fork$( (( forks_left_out == 1 )) || print -n s) left out)"
fi
(( total )) || die "no $selection in ${(j:, :)${(@)reports:t}}"

if (( $#opt_dry )); then
  print -r -- "$total $selection from ${(j:, :)${(@)reports:t}} would be mirrored into $root:"
  jq -r 'to_entries[] | [.key + 1, .value.org, .value.name, .value.url] | @tsv' \
    $tmp/selected.json |
    while IFS=$'\t' read -r idx org name url; do
      printf '%s\t%s/%s\t%s\n' $idx $org $(on_disk $name) $url
    done
  if (( forgejo )); then
    fj_orgs=(${(f)"$(jq -r '.[].org' $tmp/selected.json | sort -u)"})
    print -r -- ""
    print -r -- "and created if missing on ${fj_url:-$forgejo_default}, as ${#fj_orgs} organization(s):"
    print -r -- "  ${(j:, :)fj_orgs}"
    print -r -- "with every branch and tag of each repository pushed into it."
    if (( lfs )); then
      print -r -- "git-lfs objects would be mirrored as well."
    else
      print -r -- "git-lfs objects would not be mirrored; the pointers would go up alone."
    fi
  fi
  exit 0
fi

mkdir -p $root
export CR_FAILURES=$tmp/failures
export CR_TALLY=$tmp/tally
: > $CR_FAILURES
: > $CR_TALLY

# The organizations, once, here rather than in the workers: several workers
# creating the same organization at the same moment is a race worth not having.
export CR_FORGEJO=$forgejo
if (( forgejo )); then
  fj_init $tmp/forgejo.curlrc $fj_url || die "$FJ_ERROR"
  # Repositories whose copy on the instance is the maintained one; they are
  # cloned from github.com like any other and never pushed. See the file.
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
  fj_orgs=(${(f)"$(jq -r '.[].org' $tmp/selected.json | sort -u)"})
  print -r -- "Forgejo $FJ_URL: ${#fj_orgs} organization(s), pushing over ssh as $FJ_SSH_USER."
  typeset -a fj_failed_orgs=()
  for org in $fj_orgs; do
    if fj_ensure_org $org; then
      print -r -- "  $org: organization $FJ_ACTION"
    else
      print -ru2 -- "  $org: organization FAILED: $FJ_ERROR"
      printf '%s\t%s\t%s\t%s\n' "$org" organization 1 "$FJ_ERROR" >> $CR_FAILURES
      fj_failed_orgs+=($org)
    fi
  done
  if (( $#fj_failed_orgs )); then
    if (( $#fj_failed_orgs == $#fj_orgs )); then
      die "no organization could be created on $FJ_URL: $FJ_ERROR"
    fi
    # Nothing can be pushed into an organization that is not there, so leave
    # those repositories out rather than failing each one in turn.
    jq --argjson skip "$(printf '%s\n' $fj_failed_orgs | jq -R . | jq -sc .)" \
      '[ .[] | select($skip | index(.org) | not) ]' \
      $tmp/selected.json > $tmp/kept.json
    mv -- $tmp/kept.json $tmp/selected.json
    total=$(jq length $tmp/selected.json)
    print -ru2 -- "  skipping the ${#fj_failed_orgs} organization(s) above; $total repositories left"
    (( total )) || die "nothing left to clone"
  fi
fi

# NUL-separated, so no name could ever split into two arguments. Only the four
# fields that are never empty travel this way; see clone_one for why.
jq -j -r '
  to_entries[]
  | "\(.key + 1)\u0000\(.value.org)\u0000\(.value.name)\u0000\(.value.url)\u0000"
' $tmp/selected.json > $tmp/queue

# The sidecar the workers read the Forgejo fields from, by index: one line per
# repository, index, default branch and description, tabs and newlines squeezed
# out of the description so a line stays a line.
jq -r '
  to_entries[]
  | "\(.key + 1)\t\(.value.default_branch)\t\(.value.description | gsub("[\r\n\t]+"; " "))"
' $tmp/selected.json > $tmp/meta.tsv
export CR_META=$tmp/meta.tsv

export CR_ROOT=$root
export CR_TOTAL=$total
if (( $#opt_full || forgejo )); then
  export CR_FILTER=''
else
  export CR_FILTER='--filter=tree:0'
fi
# Never stop to ask for a credential, so a repository that was renamed or made
# private fails instead of hanging: a public repository needs none, and a
# private or internal one is cloned over ssh, where BatchMode fails rather than
# prompting if the key is not there. The Forgejo push is over ssh for the same
# reason.
export GIT_TERMINAL_PROMPT=0
export GIT_SSH_COMMAND=${GIT_SSH_COMMAND:-"ssh -o BatchMode=yes"}

if [[ -n $CR_FILTER ]]; then
  depth='commits only'
elif (( forgejo )); then
  depth='full clones, --forgejo needs the trees and blobs'
else
  depth='full clones'
fi
print -r -- "Mirroring $total $selection from ${(j:, :)${(@)reports:t}} into $root, $jobs at a time ($depth)."
started=$EPOCHREALTIME

# Each worker keeps a file saying what it is doing; the watcher prints the ones
# that have been doing it a while, so a slow clone or a push of several hundred
# tags is visible while it happens rather than only once it is over.
if (( progress )); then
  mkdir -p $tmp/inflight
  export CR_INFLIGHT=$tmp/inflight
  # trap -, because a background subshell inherits this shell's traps: a signal
  # to the process group would otherwise have the heartbeat run cleanup and
  # delete the run's temporary directory from under it. Its own two checks and
  # the kill below are what stop it.
  { unsetopt err_exit no_unset pipe_fail
    trap - EXIT INT TERM HUP
    fj_progress_watch $tmp/inflight $progress 120 $$ } &
  watcher=$!
fi

xargs -0 -n 4 -P $jobs $SELF --clone-one < $tmp/queue || :

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
if [[ -n ${CR_INFLIGHT:-} ]]; then
  for orphan in ${CR_INFLIGHT}/*(N.); do
    IFS=$'\t' read -r orphan_start orphan_slug orphan_stage orphan_pid < $orphan 2>/dev/null || continue
    print -ru2 -- "${orphan_slug:-${orphan:t}} FAILED (the worker stopped without finishing, at ${orphan_stage:-an unknown stage})"
    printf '%s\t%s\t%s\t%s\n' "${orphan_slug:-${orphan:t}}" worker 1 \
      "the worker stopped without finishing, at ${orphan_stage:-an unknown stage}" >> $CR_FAILURES
  done
fi

cloned=$(grep -c '^cloned$' $CR_TALLY || :)
updated=$(grep -c '^updated$' $CR_TALLY || :)
pushed=$(grep -c '^pushed$' $CR_TALLY || :)
changed=$(grep -c '^changed$' $CR_TALLY || :)
empty=$(grep -c '^empty$' $CR_TALLY || :)
skipped=$(grep -c '^skipped$' $CR_TALLY || :)
lfs=$(grep -c '^lfs$' $CR_TALLY || :)
actions=$(grep -c '^actions$' $CR_TALLY || :)
failed=$(grep -c . $CR_FAILURES || :)
summary="$cloned cloned, $updated updated"
if (( forgejo )); then
  summary+=", $pushed pushed to $FJ_URL ($changed changed)"
fi
if (( empty )); then
  summary+=", $empty empty"
fi
if (( skipped )); then
  summary+=", $skipped left alone"
fi
if (( lfs )); then
  summary+=", $lfs with git-lfs objects"
fi
if (( actions )); then
  summary+=", $actions with actions turned off"
fi
print -r -- "Done in $(printf '%.0fs' $(( EPOCHREALTIME - started ))): $summary, $failed failed; $(du -sh $root | cut -f1 | tr -d ' \t') in $root."

if (( failed )); then
  print -ru2 -- ""
  print -ru2 -- "Failed:"
  sort $CR_FAILURES | while IFS=$'\t' read -r slug action rc why; do
    if [[ $action == (forgejo|organization|worker) ]]; then
      print -ru2 -- "  $slug ($action): $why"
    else
      print -ru2 -- "  $slug ($action, git exit $rc): $why"
    fi
  done
  exit 1
fi
