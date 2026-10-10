#!/usr/bin/env zsh
#
# election.zsh -- the TAC election pipeline end to end, in the order the pieces
# depend on each other: export the enterprises, export their people, mirror
# every repository, then turn the mirrors and the CLOWarden configs into
# tac-eligibility-check/hashes.js.
#
#   ./election.zsh                       # the lot, activity window since 2025-10-01
#   ./election.zsh --since 2025-07-01    # a different window
#   ./election.zsh --from nominees       # pick up after the clones
#   ./election.zsh --only voters         # one step
#   ./election.zsh -n                    # print the commands and stop
#
# Steps: repos, people, clones, mirror, nominees, voters.
#
# Every step is re-runnable. The exports overwrite, the clones fetch rather than
# re-clone, and each tac-eligibility phase writes its own file under repos/, so
# an interrupted run resumes from --from. The expensive step is `nominees`: it
# asks GitHub who each commit address belongs to, and only repos/cache.json
# keeps a second run from paying for it again.
#
# Nothing here commits anything. The last step writes exactly one file inside
# the tac-eligibility-check checkout -- hashes.js, no git commands -- so
# reviewing and committing it is a hand step.

setopt err_exit no_unset pipe_fail
zmodload zsh/datetime

SELF=${0:A}
# Every path below is relative to the repository, so the script runs the same
# from anywhere.
cd ${SELF:h}

BIN=${BIN:-target/release}

RESULTS=results            # gh-enterprise-repos exports
PEEPS=peeps                # gh-org-members exports
REPOS=repos                # the bare mirrors, and the phase files
MIRROR=mirror              # the CLOWarden config checkouts
WATCHED=reports/clowarden-watched-repos.csv
OUT=tac-eligibility-check/hashes.js

ENTERPRISES=(lf-decentralized-trust openwallet-foundation)

# Both lists span both enterprises. The reports scope the nominee sweep to the
# repositories they name; the people exports are merged for the voter roll --
# one export covers one enterprise, and someone who turns up in both is one
# voter whose `where` names every place that put them there.
REPORTS=($RESULTS/${^ENTERPRISES}.yaml)
PEOPLE=($PEEPS/${^ENTERPRISES}.yaml)

SINCE=2025-10-01

STEPS=(repos people clones mirror nominees voters)

usage() {
  print -r -- "usage: ${SELF:t} [--since DATE] [--from STEP] [--only STEP] [-n]

  --since DATE  start of the activity window (default: $SINCE)
  --from STEP   start at this step and run the rest
  --only STEP   run just this step; repeatable
  -n            print the commands without running them

  steps, in order: ${(j: :)STEPS}"
}

die() {
  print -ru2 -- "${SELF:t}: $1"
  exit ${2:-1}
}

zparseopts -D -E -F -- -since:=opt_since -from:=opt_from -only+:=opt_only \
  n=opt_dry h=opt_help -help=opt_help || { usage >&2; exit 2 }
if (( $#opt_help )); then
  usage
  exit 0
fi
[[ ${1:-} == -- ]] && shift
(( $# == 0 )) || { usage >&2; exit 2 }

(( $#opt_since )) && SINCE=$opt_since[2]
[[ $SINCE == [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9] ]] \
  || die "--since takes a YYYY-MM-DD date, not ${(q)SINCE}"

# Which steps to run.
typeset -A want
for step in $STEPS; do want[$step]=1; done

if (( $#opt_only )); then
  for step in $STEPS; do want[$step]=0; done
  for (( i = 2; i <= $#opt_only; i += 2 )); do
    step=$opt_only[i]
    (( ${+want[$step]} )) || die "no step called ${(q)step}; one of: ${(j:, :)STEPS}"
    want[$step]=1
  done
fi

if (( $#opt_from )); then
  (( $#opt_only )) && die "--from and --only are two ways to say the same thing; pass one"
  from=$opt_from[2]
  (( ${+want[$from]} )) || die "no step called ${(q)from}; one of: ${(j:, :)STEPS}"
  for step in $STEPS; do
    [[ $step == $from ]] && break
    want[$step]=0
  done
fi

# --- what has to be in place before a long run starts ------------------------

missing() {
  if (( $#opt_dry )); then
    print -ru2 -- "${SELF:t}: $1"
  else
    die "$1"
  fi
}

for tool in gh-enterprise-repos gh-org-members tac-eligibility; do
  [[ -x $BIN/$tool ]] || missing "$BIN/$tool is not there; run: make release"
done
if (( want[clones] )) && [[ ! -x ./clone-repos.zsh ]]; then
  missing "./clone-repos.zsh is not executable"
fi
if (( want[mirror] )); then
  for script in ./find-clowarden-configs.zsh ./mirror-clowarden.zsh; do
    [[ -x $script ]] || missing "$script is not executable"
  done
fi
# Fail now rather than after the sweep: the write phase needs the checkout.
if (( want[voters] )) && [[ ! -d ${OUT:h} ]]; then
  missing "no ${OUT:h}/ checkout to write ${OUT:t} into; clone it here first"
fi
if (( want[voters] )) && ! (( want[people] )); then
  for people in $PEOPLE; do
    [[ -r $people ]] \
      || missing "no $people; run the people step, or edit PEOPLE in this script"
  done
fi

# --- the steps ---------------------------------------------------------------

# Print a command, and run it unless this is a dry run. err_exit stops the
# script on the first failure, which is what a pipeline of dependent steps
# wants.
run() {
  print -r -- "+ $*"
  (( $#opt_dry )) && return 0
  "$@"
}

banner() {
  print -r -- ""
  print -r -- "===== $1"
}

step_repos() {
  banner "repositories of ${(j:, :)ENTERPRISES} -> $RESULTS/"
  local -a args=()
  local enterprise
  for enterprise in $ENTERPRISES; do
    args+=(-e $enterprise)
  done
  run $BIN/gh-enterprise-repos $args -d $RESULTS
}

step_people() {
  banner "people and teams -> $PEEPS/"
  (( $#opt_dry )) || mkdir -p $PEEPS
  local enterprise
  for enterprise in $ENTERPRISES; do
    run $BIN/gh-org-members -e $enterprise -o $PEEPS/$enterprise.yaml
  done
}

step_clones() {
  banner "bare mirrors -> $REPOS/"
  local report
  for report in $REPORTS; do
    [[ -r $report ]] || missing "cannot read $report; run the repos step first"
  done
  run ./clone-repos.zsh -d $REPOS $REPORTS
}

# Which repository holds each organization's config, then the checkouts
# themselves. The list is rebuilt every run rather than cached: an organization
# added since the last election declares maintainers too, and a stale list
# drops them silently.
step_mirror() {
  banner "CLOWarden configs -> $MIRROR/"
  local report
  for report in $REPORTS; do
    [[ -r $report ]] || missing "cannot read $report; run the repos step first"
  done
  run ./find-clowarden-configs.zsh -o $WATCHED $REPORTS
  run ./mirror-clowarden.zsh $WATCHED $MIRROR
}

# `--flag A --flag B`, one occurrence per file, however many there are.
repeated() {
  local flag=$1 value
  shift
  for value in "$@"; do
    print -r -- $flag
    print -r -- $value
  done
}

# Nominees: who authored a commit in the window, with the addresses resolved to
# accounts. The only step that talks to the API.
step_nominees() {
  banner "nominees: commit activity since $SINCE"
  run $BIN/tac-eligibility ${(f)"$(repeated --report $REPORTS)"} --since $SINCE \
    --repos $REPOS --phase authors --phase logins
}

# Voters: who the CLOWarden configs and the export make a maintainer or owner --
# then both lists into hashes.js.
step_voters() {
  banner "voters: maintainers and owners, then $OUT"
  run $BIN/tac-eligibility ${(f)"$(repeated --report $REPORTS)"} --since $SINCE \
    --repos $REPOS --phase voters --phase write \
    ${(f)"$(repeated --people $PEOPLE)"} --clowarden $MIRROR --out $OUT
}

# --- run ---------------------------------------------------------------------

print -r -- "TAC election pipeline in ${SELF:h:t}: window since $SINCE, binaries from $BIN/"
running=()
for step in $STEPS; do
  (( want[$step] )) && running+=($step)
done
(( $#running )) || die "no steps left to run"
print -r -- "Running: ${(j: :)running}"
started=$EPOCHREALTIME

for step in $STEPS; do
  (( want[$step] )) || continue
  step_start=$EPOCHREALTIME
  step_$step
  (( $#opt_dry )) || print -r -- "  $step done in $(printf '%.0fs' $(( EPOCHREALTIME - step_start )))"
done

if (( $#opt_dry )); then
  print -r -- ""
  print -r -- "Dry run: nothing above was executed."
  exit 0
fi

print -r -- ""
print -r -- "Pipeline done in $(printf '%.0fs' $(( EPOCHREALTIME - started )))."
if (( want[voters] )); then
  print -r -- "$OUT is written and nothing was committed: review it in ${OUT:h}/ and"
  print -r -- "commit it there yourself. The logins behind the hashes stay in $REPOS/, which"
  print -r -- "is gitignored -- the published file holds only digests."
fi
