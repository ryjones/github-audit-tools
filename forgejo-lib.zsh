#!/usr/bin/env zsh
#
# forgejo-lib.zsh -- the Forgejo half of clone-repos.zsh and update-repos.zsh:
# the API calls that create an organization and a repository, the push that
# fills one from a bare clone, and the refetch that turns a commits-only mirror
# into one with trees and blobs. It also keeps the small progress record both
# scripts read to say, every few seconds, which repositories are still working
# and on what.
#
# Sourced, never run. Both scripts source it from beside themselves.
#
# What it deliberately does not do: Forgejo's own mirror feature. A pull mirror
# would make the instance fetch from github.com on a schedule, keep the repo
# read-only, and die the day the network or the upstream does. These are plain
# repositories, pushed branch for branch and tag for tag from the local clones,
# so they stand on their own once they are there.
#
# Two credentials, two protocols, on purpose:
#   * the REST API over https, with the instance token -- creating the
#     organization and the repository, and setting the default branch;
#   * git over ssh, with the key registered on the instance -- the push itself.
#     https pushes would need the token on a command line, where `ps` shows it.
#
# Environment:
#   FORGEJO_URL         the instance (default https://forgejo.lan.ryjones.dev)
#   FORGEJO_TOKEN       the API token, or
#   FORGEJO_TOKEN_FILE  a file holding it (default ~/.config/forgejo/token.txt)
#   FORGEJO_SSH_USER    the ssh user to push as (default git)
#   FORGEJO_SKIP_FILE   repositories never to push into, one ORG/NAME per line
#                       (default forgejo-skip.txt beside the scripts)
#   FJ_LFS_ENABLED      1 to mirror git-lfs objects at all; 0, the default,
#                       does none of that work (both scripts' --lfs sets it)
#   FJ_LFS_ALWAYS       1 to do the git-lfs work even when the instance's
#                       branches and tags are already exactly the clone's
#
# fj_init exports what it resolved, so a worker re-invoked through xargs
# inherits it and needs no second look at the token.

zmodload zsh/datetime

typeset -g FJ_STATUS= FJ_BODY= FJ_ACTION= FJ_ERROR= FJ_NOTE= FJ_PUSHED=0 FJ_CHANGED=0
typeset -g FJ_STAGE_FILE= FJ_STAGE_LABEL= FJ_STAGE_START=0
typeset -g FJ_SKIPPED=0 FJ_DIVERGED= FJ_LFS=0 FJ_RETRIED=0 FJ_ACTIONS=0

# fj_init CONFIG_PATH [URL] -- read the token into a curl config file at
# CONFIG_PATH and work out the API and ssh endpoints. Call once, in the parent.
fj_init() {
  local config=$1 url=${2:-${FORGEJO_URL:-https://forgejo.lan.ryjones.dev}}
  local token= file=${FORGEJO_TOKEN_FILE:-$HOME/.config/forgejo/token.txt}

  (( $+commands[curl] )) || { FJ_ERROR="curl is required and was not found"; return 1 }

  if [[ -n ${FORGEJO_TOKEN:-} ]]; then
    token=$FORGEJO_TOKEN
  elif [[ -r $file ]]; then
    token=$(tr -d ' \t\r\n' < $file)
  else
    FJ_ERROR="no Forgejo token: set FORGEJO_TOKEN, or FORGEJO_TOKEN_FILE to a file holding one ($file is not readable)"
    return 1
  fi
  [[ -n $token ]] || { FJ_ERROR="the Forgejo token in $file is empty"; return 1 }

  FJ_URL=${url%%/}
  FJ_API=$FJ_URL/api/v1
  FJ_HOST=${${FJ_URL#*://}%%/*}
  FJ_SSH_USER=${FORGEJO_SSH_USER:-git}
  FJ_CURL_CONFIG=$config

  # The token lives in a 0600 config file rather than in an argument, because
  # an argument is in the process list for every account on the machine.
  local before=$(umask)
  umask 077
  print -r -- "header = \"Authorization: token $token\"" > $FJ_CURL_CONFIG
  umask $before

  export FJ_URL FJ_API FJ_HOST FJ_SSH_USER FJ_CURL_CONFIG
}

# fj_require -- in a worker, confirm fj_init ran in the parent.
fj_require() {
  [[ -n ${FJ_API:-} && -r ${FJ_CURL_CONFIG:-/nonexistent} ]] && return 0
  FJ_ERROR="forgejo settings did not reach this worker"
  return 1
}

# ---------------------------------------------------------------- progress ---
#
# Each worker keeps one little file saying what it started, when, and what it
# is doing now; the parent prints the ones that have been at it a while. None
# of it is Forgejo's business, but both scripts want it and both source this.

# The record carries the worker's pid as a fourth field, so a stage file whose
# worker is gone can be told from one that is still being worked on. A worker
# killed outright -- a zsh error under `no_unset`, the OOM killer, kill -9 --
# runs no fj_stage_done, and the file it leaves behind used to be read as a
# repository still working: on 2026-10-22 hyperledger-indy/indy-vdr was
# reported as having spent 6m03s "setting the default branch" after its worker
# had already died at that line. The watcher skips those, and both scripts
# count what is left at the end as a failure.

# fj_stage_init FILE LABEL -- begin the record for one repository.
fj_stage_init() {
  FJ_STAGE_FILE=$1
  FJ_STAGE_LABEL=$2
  FJ_STAGE_START=$EPOCHSECONDS
  fj_stage starting
}

# fj_stage TEXT -- what this repository is doing now. A no-op until
# fj_stage_init has named a file, so callers need no conditionals.
fj_stage() {
  [[ -n ${FJ_STAGE_FILE:-} ]] || return 0
  # print -r keeps the text as it is, so the separators are written as the
  # tabs the parent splits on rather than as backslash-t.
  print -r -- "$FJ_STAGE_START"$'\t'"$FJ_STAGE_LABEL"$'\t'"$1"$'\t'"$$" > $FJ_STAGE_FILE 2>/dev/null || :
  return 0
}

# fj_stage_done -- finished; the parent should stop naming this one.
fj_stage_done() {
  [[ -n ${FJ_STAGE_FILE:-} ]] || return 0
  rm -f -- $FJ_STAGE_FILE
  FJ_STAGE_FILE=
  return 0
}

# fj_elapsed SECONDS -- 48s, 1m20s, 1h04m. No newline: it goes inside a line.
fj_elapsed() {
  local s=$1
  if (( s < 60 )); then
    printf '%ds' $s
  elif (( s < 3600 )); then
    printf '%dm%02ds' $(( s / 60 )) $(( s % 60 ))
  else
    printf '%dh%02dm' $(( s / 3600 )) $(( (s % 3600) / 60 ))
  fi
}

# fj_progress_watch DIR INTERVAL [CAP] [PARENT] -- every INTERVAL seconds, one
# line for each repository that has been working longer than that: how long it
# has been at it, which repository, and what it is doing. Longest first, and
# all of them rather than the first three and a count -- what any of them is
# doing is the useful part, and -j bounds how many there can be to one per
# worker. Run it in the background beside xargs and kill it afterwards. A run
# of small repositories never trips it, so nothing is printed at all.
#
# The wait doubles, up to CAP seconds, for as long as the same repositories are
# doing the same thing -- a push of a thousand tags took an hour and said so
# 130 times at a flat 30 seconds. Any change in that set, including one of them
# finishing, starts the wait over, so the next thing to happen is named at the
# original interval rather than CAP seconds later.
#
# It also stops on its own when PARENT is gone or DIR has been removed. The
# caller kills it, but a caller killed outright cannot, and a heartbeat that
# outlives its run sleeps on forever talking to a terminal nobody is reading.
fj_progress_watch() {
  local dir=$1 interval=$2 cap=${3:-120} parent=${4:-0} wait=$2
  local now= start= label= stage= pid= f= sig= last= width=0 line= el= slug=
  local -a lines=() names=() out=() fields=()
  if (( cap < interval )); then
    cap=$interval
  fi
  while sleep $wait; do
    [[ -d $dir ]] || break
    if (( parent )) && ! kill -0 $parent 2>/dev/null; then
      break
    fi
    now=$EPOCHSECONDS
    lines=()
    names=()
    width=0
    for f in $dir/*(N.); do
      IFS=$'\t' read -r start label stage pid < $f 2>/dev/null || continue
      [[ -n ${start:-} && $start == <-> ]] || continue
      (( now - start >= interval )) || continue
      # Its worker is gone, so it is not working on anything. The script says
      # so once the run is over; a line per tick saying it again would not help.
      if [[ -n ${pid:-} && $pid == <-> ]] && ! kill -0 $pid 2>/dev/null; then
        continue
      fi
      # Seconds first, for the sort. The line itself waits until the widest
      # name in this batch is known, so the stages line up under each other.
      lines+=("$(( now - start ))"$'\t'"$label"$'\t'"${stage:-working}")
      names+=("$label ${stage:-working}")
      if (( ${#label} > width )); then
        width=${#label}
      fi
    done
    if (( ! $#lines )); then
      # Nothing slow enough to mention; be ready to notice the next one at once.
      wait=$interval
      last=
      continue
    fi
    lines=(${(f)"$(print -rl -- $lines | sort -t$'\t' -k1,1nr)"})
    out=()
    for line in $lines; do
      fields=(${(ps:\t:)line})
      el=$(fj_elapsed $fields[1])
      slug=$fields[2]
      out+=("  … ${(l:6:)el}  ${(r:$width:)slug}  ${fields[3]:-working}")
    done
    # One write, so a dozen lines are not interleaved with the workers' own.
    print -rl -- $out
    sig=${(j:|:)${(o)names}}
    if [[ $sig == "$last" ]]; then
      wait=$(( wait * 2 ))
      if (( wait > cap )); then
        wait=$cap
      fi
    else
      wait=$interval
    fi
    last=$sig
  done
}

# -------------------------------------------------------------------- API ---

# fj_api METHOD PATH [JSON] -- one REST call. Sets FJ_STATUS and FJ_BODY, and
# returns non-zero on anything but a 2xx, so it reads as a condition.
fj_api() {
  # Not `path`: zsh ties a lowercase `path` to $PATH, and assigning a REST
  # route to it empties the command search path for the rest of the function.
  local method=$1 route=$2 data=${3:-}
  local body=$(mktemp) code=
  local -a args=(--silent --show-error --config $FJ_CURL_CONFIG
                 --output $body --write-out '%{http_code}' --request $method)

  if [[ -n $data ]]; then
    args+=(--header 'Content-Type: application/json' --data-binary @-)
    code=$(print -rn -- $data | curl $args -- $FJ_API$route 2>> $body) || code=000
  else
    code=$(curl $args -- $FJ_API$route 2>> $body) || code=000
  fi

  FJ_STATUS=$code
  FJ_BODY=$(< $body)
  rm -f -- $body
  [[ $code == 2?? ]]
}

# fj_message -- the last call's error in one line: Forgejo's own `message` when
# it sent JSON, otherwise whatever came back, trimmed.
fj_message() {
  local msg=
  msg=$(print -r -- $FJ_BODY | jq -r 'if type == "object" then (.message // .error // empty) else empty end' 2>/dev/null) || msg=
  [[ -n $msg ]] || msg=$(print -r -- $FJ_BODY | tr '\n' ' ' | cut -c1-200)
  print -r -- "HTTP $FJ_STATUS ${msg:-no output}"
}

# fj_ensure_org ORG -- the organization, created public if it is not there.
# Sets FJ_ACTION to `created` or `present`.
fj_ensure_org() {
  local org=$1
  if fj_api GET /orgs/$org; then FJ_ACTION=present; return 0; fi
  if [[ $FJ_STATUS != 404 ]]; then
    FJ_ERROR="GET /orgs/$org: $(fj_message)"
    return 1
  fi
  if fj_api POST /orgs "$(jq -nc --arg u $org '{username: $u, visibility: "public"}')"; then
    FJ_ACTION=created
    return 0
  fi
  # Another run, or another worker, may have created it a moment ago.
  if fj_api GET /orgs/$org; then FJ_ACTION=present; return 0; fi
  FJ_ERROR="POST /orgs $org: $(fj_message)"
  return 1
}

# fj_ensure_repo ORG NAME [DESCRIPTION] -- the repository, created empty and
# public so the push can fill it. Sets FJ_ACTION to `created` or `present`.
fj_ensure_repo() {
  local org=$1 name=$2 desc=${3:-}
  if fj_api GET /repos/$org/$name; then FJ_ACTION=present; return 0; fi
  if [[ $FJ_STATUS != 404 ]]; then
    FJ_ERROR="GET /repos/$org/$name: $(fj_message)"
    return 1
  fi
  local payload=$(jq -nc --arg n $name --arg d "$desc" \
    '{name: $n, description: $d, private: false, auto_init: false}')
  if fj_api POST /orgs/$org/repos $payload; then FJ_ACTION=created; return 0; fi
  if fj_api GET /repos/$org/$name; then FJ_ACTION=present; return 0; fi
  FJ_ERROR="POST /orgs/$org/repos $name: $(fj_message)"
  return 1
}

# fj_actions_off ORG NAME [BODY] -- the repository's Actions unit, off.
#
# A mirror's workflows are github.com's. Pushed into a repository with the unit
# on, every branch and every tag of them creates workflow runs here: the
# instance held 196,977 of them on 2026-10-22, 149,911 of those belonging to
# one mirrored repository, with 308 `schedule:` triggers registered against
# runners that were never meant to build any of it. Nothing here wants that
# built, and nobody is reading the failures.
#
# The unit is the only switch Forgejo has -- there is no per-workflow one -- and
# turning it off takes workflow_dispatch with it, which a mirror has no use for
# either. A repository named in forgejo-skip.txt never reaches this function,
# so a copy on the instance that something else maintains keeps what it has.
#
# BODY is the repository JSON already in hand, to save a GET. Sets FJ_ACTIONS
# when this call was the one that turned the unit off, so the first run says so
# and later ones are quiet. Schedules already registered are rows in the
# instance's database and are not undone by this; see docs/forgejo-mirror.md.
fj_actions_off() {
  local org=$1 name=$2 body=${3:-} has=
  has=$(print -r -- $body | jq -r 'if type == "object" then (.has_actions | tostring) else "" end' 2>/dev/null) || has=
  if [[ $has != (true|false) ]]; then
    if ! fj_api GET /repos/$org/$name; then
      FJ_ERROR="GET /repos/$org/$name: $(fj_message)"
      return 1
    fi
    has=$(print -r -- $FJ_BODY | jq -r '.has_actions | tostring' 2>/dev/null) || has=
  fi
  # An instance that will not say is not one to guess about: leaving the unit
  # on by accident is how the 196,977 happened.
  if [[ $has != (true|false) ]]; then
    FJ_ERROR="GET /repos/$org/$name said nothing about has_actions"
    return 1
  fi
  [[ $has == true ]] || return 0
  if ! fj_api PATCH /repos/$org/$name "$(jq -nc '{has_actions: false}')"; then
    FJ_ERROR="PATCH /repos/$org/$name has_actions=false: $(fj_message)"
    return 1
  fi
  FJ_ACTIONS=1
  return 0
}

# fj_remote_url ORG NAME -- where the push goes.
fj_remote_url() {
  print -r -- "ssh://$FJ_SSH_USER@$FJ_HOST/$1/$2.git"
}

# fj_has_refs DIR -- true when the clone holds a branch or a tag to push.
fj_has_refs() {
  [[ -n $(git -C $1 for-each-ref --count=1 --format=x refs/heads refs/tags) ]]
}

# fj_is_partial DIR -- true for a clone fetched with --filter, whose objects
# are a promise rather than a fact. Pushing one would have git fetch every
# missing tree back from github.com first, repository by repository.
fj_is_partial() {
  local dir=$1
  if [[ -n $(git -C $dir config --get remote.origin.partialclonefilter 2>/dev/null || :) ]]; then
    return 0
  fi
  [[ -n $(git -C $dir config --get extensions.partialclone 2>/dev/null || :) ]]
}

# fj_backfill DIR -- turn a commits-only clone into a whole one: drop the
# filter, then refetch, which skips negotiation and brings what a fresh clone
# would have had. Slow and large; it is why --forgejo implies --full.
fj_backfill() {
  local dir=$1
  git -C $dir config --unset-all remote.origin.partialclonefilter 2>/dev/null || :
  git -C $dir config --unset-all remote.origin.promisor 2>/dev/null || :
  git -C $dir fetch --refetch --prune --quiet || return $?
  git -C $dir config --unset-all extensions.partialclone 2>/dev/null || :
  return 0
}

# git-lfs, which is off unless FJ_LFS_ENABLED is 1. A mirror clone copies the
# pointer blobs and not one byte of what they stand for, so mirroring a
# repository that uses it means two more transfers than the others: the objects
# down from github.com, then up to the instance.
#
# Those two transfers are the most expensive thing either script does, for a
# copy of github.com nobody checks out -- 96 objects on the instance were 4.7
# of its 12 GiB of git-lfs on 2026-10-22, and the local clones held 10.4 GiB
# more -- so by default neither is made. What is pushed then is what a bare
# mirror clone holds: the pointer files, with nothing behind them. A checkout
# of such a repository gets the pointer text in place of the file, which is the
# bargain FJ_LFS_ENABLED=1 is there to undo. purge-lfs.zsh takes back the
# objects an earlier run with it on left behind.
#
# The push itself does not care either way: fj_push passes --no-verify, so
# git-lfs's pre-push hook never runs and never refuses a push whose objects are
# not there.
#
# Both transfers are done here rather than left to git-lfs's own pre-push hook.
# That hook is ambient: `git lfs install` writes it into whatever
# core.hooksPath points at, so whether a mirror push uploads its objects would
# otherwise depend on how the machine running this happens to be set up. The
# push below passes --no-verify for the same reason.

# fj_lfs_available -- true when git-lfs is installed at all. Without it a
# repository that uses it cannot even be recognized as one.
fj_lfs_available() {
  git lfs version > /dev/null 2>&1
}

# fj_lfs_files DIR -- one line per git-lfs pointer any ref of the clone holds,
# and nothing at all for the great majority of repositories, which hold none.
# The scan walks every ref, so ask once and keep the answer.
fj_lfs_files() {
  git -C $1 lfs ls-files --all 2>/dev/null || :
}

# fj_lfs_fetch DIR -- bring down from github.com every object those pointers
# name, for every ref rather than just the checked-out one.
fj_lfs_fetch() {
  git -C $1 lfs fetch --all
}

# fj_lfs_push DIR ORG NAME -- send those objects to the instance, before the
# refs that point at them. locksverify off: the instance answers a lock query
# per batch, and file locks mean nothing to a mirror nobody commits to.
fj_lfs_push() {
  local dir=$1 org=$2 name=$3
  git -C $dir -c lfs.locksverify=false lfs push --all $(fj_remote_url $org $name)
}

# A connection that was refused before it was a connection. The push goes over
# ssh, and sshd turns away whatever is over its MaxStartups limit before
# authentication -- the stock 10:30:100 starts refusing at random above ten
# pre-auth connections, and one worker per core opening two of them per
# repository (an ls-remote, then the push) passes that easily. A reload of
# sshd, or a blink in the network, looks the same from here. None of it says
# anything about the repository, and the next attempt usually works, so a push
# that failed this way is tried again rather than reported.
#
# ssh's own words are what give it away: git's four closing lines about access
# rights and the repository existing are printed whether the connection was
# dropped or the key was wrong, so they distinguish nothing. Permission denied
# is deliberately not in here -- a key the instance does not know will not
# start working on the third try.
typeset -g FJ_TRANSPORT_RE='kex_exchange_identification|ssh_exchange_identification|^Connection (closed|reset|timed out)|Connection closed by|Connection reset by|Connection timed out|Connection refused|No route to host|Temporary failure in name resolution|banner exchange'

# fj_transport_error FILE -- the first line in which ssh said the connection
# never happened, and nothing at all when there is none. Both scripts'
# why_failed asks first, because their own rule -- the last line that said
# something -- picks `and the repository exists.` out of a dropped push, which
# is advice about a repository that is in fact there.
fj_transport_error() {
  [[ -n ${1:-} && -r $1 ]] || return 0
  grep -m1 -E $FJ_TRANSPORT_RE $1 2>/dev/null || :
}

# fj_push DIR ORG NAME PORCELAIN [ERRORS] -- every branch and every tag,
# forced, with anything the upstream no longer has pruned off the Forgejo side.
# refs/pull/* is left behind on purpose: GitHub advertises thousands of them,
# and Forgejo keeps its own pull requests under that namespace.
#
# git's stderr goes to ERRORS, which is the file the caller is collecting it
# in; the redirect is done here rather than by the caller so that each attempt
# can be read before it is kept.
#
# A push whose connection was dropped is tried again, FJ_PUSH_TRIES times in
# all (3 by default), waiting two seconds and then four. FJ_RETRIED is how many
# retries it took, for the caller to report. Anything git itself refused is
# reported at once instead.
#
# --no-verify: the hooks a mirror push would run are none of its business, and
# the one that is usually there -- git-lfs's pre-push, installed globally --
# would upload the objects a second time, or abort the push over objects
# fj_lfs_push has already dealt with. See the git-lfs section above.
# fj_retrying WHAT SLUG ERRORS COMMAND... -- run COMMAND, and run it again when
# it failed with a connection that never happened, FJ_PUSH_TRIES times in all
# (3 by default), waiting two seconds and then four. WHAT fills in the sentence
# "<slug>: <ssh said this>; <what> again in 2s", so it is a gerund.
#
# COMMAND's stderr is collected in ERRORS, which is the file the caller keeps
# git's words in; the redirect is here rather than at the call site so that
# each attempt can be read before it is kept. Its stdout is left alone, so a
# caller redirects that itself. FJ_RETRIED counts the retries since fj_sync
# last reset it, for the caller to report.
fj_retrying() {
  local what=$1 slug=$2 errors=${3:-/dev/null}
  shift 3
  local tries=${FJ_PUSH_TRIES:-3} attempt=1 rc=0 pause=2 dropped=
  local err=$(mktemp)
  while :; do
    rc=0
    "$@" 2> $err || rc=$?
    cat $err >> $errors
    (( rc )) || break
    dropped=$(fj_transport_error $err)
    [[ -n $dropped ]] || break
    (( attempt < tries )) || break
    print -ru2 -- "    $slug: $dropped; $what again in ${pause}s (retry $attempt of $(( tries - 1 )))"
    fj_stage "waiting ${pause}s to try $what again after a dropped connection"
    sleep $pause
    attempt=$(( attempt + 1 ))
    pause=$(( pause * 2 ))
    : > $err
  done
  FJ_RETRIED=$(( FJ_RETRIED + attempt - 1 ))
  rm -f -- $err
  return $rc
}

fj_push() {
  local dir=$1 org=$2 name=$3 porcelain=$4 errors=${5:-/dev/null}
  fj_retrying pushing "$org/$name" $errors \
    git -C $dir push --no-verify --porcelain --prune $(fj_remote_url $org $name) \
      '+refs/heads/*:refs/heads/*' '+refs/tags/*:refs/tags/*' > $porcelain
}

# fj_ls_remote DIR ORG NAME OUT [ERRORS] -- the branches and tags the instance
# holds, into OUT, with the same retry. Non-zero when they could not be read at
# all, and then git's own words are in ERRORS.
fj_ls_remote() {
  local dir=$1 org=$2 name=$3 out=$4 errors=${5:-/dev/null}
  fj_retrying "listing the instance's refs" "$org/$name" $errors \
    git -C $dir ls-remote --heads --tags $(fj_remote_url $org $name) > $out
}

# fj_push_changed PORCELAIN -- how many refs that push actually moved. A
# porcelain line is FLAG<tab>FROM:TO<tab>SUMMARY, and `=` means up to date.
fj_push_changed() {
  grep -cE $'^[ +*-]\t' $1 2>/dev/null || :
}

# fj_push_summary PORCELAIN -- what that push did, for the progress line.
fj_push_summary() {
  local changed=$(fj_push_changed $1)
  if (( changed )); then
    print -r -- "$changed ref$( (( changed == 1 )) || print -n s) updated"
  else
    print -r -- "up to date"
  fi
}

# fj_ensure_default_branch ORG NAME [BRANCH] -- Forgejo names the first branch
# pushed into an empty repository as the default, which with many branches
# arriving at once is whichever git sent first. Say which it should be.
fj_ensure_default_branch() {
  # ${3:-}, not $3: fj_default_branch answers nothing for a clone whose HEAD
  # names a branch that is not there, and under `no_unset` reading $3 then
  # killed the worker outright -- mid-run, after the push, with no line and no
  # failure recorded for that repository. The caller quotes the argument too;
  # either alone is enough, and this is not a thing to be wrong about twice.
  local org=$1 name=$2 branch=${3:-} current=
  [[ -n $branch ]] || return 0
  if fj_api GET /repos/$org/$name; then
    current=$(print -r -- $FJ_BODY | jq -r '.default_branch // ""' 2>/dev/null) || current=
    if [[ $current == "$branch" ]]; then
      return 0
    fi
  fi
  if fj_api PATCH /repos/$org/$name "$(jq -nc --arg b $branch '{default_branch: $b}')"; then
    return 0
  fi
  FJ_ERROR="PATCH /repos/$org/$name default_branch=$branch: $(fj_message)"
  return 1
}

# fj_skip_init [FILE] -- name the list of repositories never to push into, and
# export it so a worker re-invoked through xargs reads the same one. Returns
# non-zero when there is no list, which is not an error.
fj_skip_init() {
  local file=${1:-${FORGEJO_SKIP_FILE:-}}
  FJ_SKIP_FILE=
  [[ -n $file && -r $file ]] || return 1
  FJ_SKIP_FILE=$file
  export FJ_SKIP_FILE
  return 0
}

# fj_skip_count -- how many repositories that list names.
fj_skip_count() {
  [[ -n ${FJ_SKIP_FILE:-} && -r ${FJ_SKIP_FILE:-/nonexistent} ]] || { print -r -- 0; return 0 }
  grep -cE '^[[:space:]]*[^#[:space:]]' $FJ_SKIP_FILE || print -r -- 0
}

# fj_skipped ORG NAME -- true when the list names this repository. Read per
# repository rather than held in an array, because an associative array cannot
# be exported to the workers.
fj_skipped() {
  [[ -n ${FJ_SKIP_FILE:-} && -r ${FJ_SKIP_FILE:-/nonexistent} ]] || return 1
  local slug=${(L)1}/${(L)2} line=
  while IFS= read -r line || [[ -n $line ]]; do
    line=${line%%\#*}
    line=${line//[[:space:]]/}
    [[ -n $line ]] || continue
    if [[ ${(L)line} == $slug ]]; then
      return 0
    fi
  done < $FJ_SKIP_FILE
  return 1
}

# fj_diverged DIR ORG NAME REFS [ERRORS] -- whether the repository on the
# instance holds a branch or tag pointing at an object the clone does not have:
# work that exists only there, which this push would force away and prune.
# REFS is a file the caller owns; the instance's refs are left in it, because
# what they are is worth knowing twice (see fj_refs_match).
#
# Three answers, not two:
#
#   0  it does -- FJ_DIVERGED names the first such ref
#   1  it does not
#   2  nobody knows: its refs could not be read at all, and FJ_ERROR says so
#
# The third one used to be answered as the second. ls-remote's failure went to
# /dev/null and a non-zero exit was read as `no divergence`, so a push that
# forces every branch and prunes everything else went ahead on the strength of
# a question that had never been asked -- and a connection the instance dropped
# pre-auth was enough to cause it. That is the accident forgejo-skip.txt was
# written for, arrived at from the other direction. A caller must treat 2 as a
# failure and leave the repository for the next run.
fj_diverged() {
  local dir=$1 org=$2 name=$3 refs=$4 errors=${5:-/dev/null}
  local line= sha=
  FJ_DIVERGED=
  if ! fj_ls_remote $dir $org $name $refs $errors; then
    FJ_ERROR="could not read the refs already on $(fj_remote_url $org $name)"
    return 2
  fi
  while IFS= read -r line || [[ -n $line ]]; do
    sha=${line%%[[:space:]]*}
    [[ -n $sha ]] || continue
    # GIT_NO_LAZY_FETCH, because a commits-only clone would otherwise ask
    # github.com for an object that only the instance has.
    if ! GIT_NO_LAZY_FETCH=1 git -C $dir cat-file -e "$sha^{commit}" 2>/dev/null; then
      FJ_DIVERGED=${line##*[[:space:]]}
      return 0
    fi
  done < $refs
  return 1
}

# fj_refs_match DIR REFS -- true when the instance's branches and tags are
# exactly the clone's: the same names at the same objects, with none left over
# on either side. Then this run has nothing to send, and nothing to prune.
#
# Worth asking before the git-lfs work, because that is where the time goes.
# `git lfs ls-files --all` walks every ref of the repository, and `git lfs push
# --all` then re-uploads every object it found whether the instance has it or
# not: on 2026-10-22 hiero-ledger/solo, which holds one object, spent 2m38s
# sending the same 123 MB it had sent the run before, and
# LFDT-Lineth/lineth-monorepo did it 1,466 times over. Forgejo's batch endpoint
# does not spare the client that, so the only cheap answer is not to start.
#
# The peeled lines ls-remote prints for annotated tags (`refs/tags/v1^{}`, 82
# of besu's 467) have no counterpart in for-each-ref and are dropped.
fj_refs_match() {
  local dir=$1 refs=$2
  local mine=$(mktemp) theirs=$(mktemp) rc=0
  git -C $dir for-each-ref --format='%(objectname) %(refname)' refs/heads refs/tags \
    | sort > $mine
  grep -v '\^{}$' $refs | tr '\t' ' ' | sort > $theirs || :
  cmp -s $mine $theirs || rc=1
  rm -f -- $mine $theirs
  return $rc
}

# fj_sync DIR ORG NAME DEFAULT_BRANCH DESCRIPTION ERRORS -- everything the
# Forgejo side of one repository needs: the repository itself, trees and blobs
# if the clone was fetched without them, every branch and tag, and the default
# branch GitHub advertises. The organization is not made here: the scripts make
# those once, up front, because several workers creating the same organization
# at the same moment is a race worth not having.
#
# Sets FJ_NOTE for the caller's progress line and FJ_PUSHED when refs were
# actually sent; on failure, sets FJ_ERROR and leaves git's own words in
# ERRORS, which is a file the caller is collecting stderr in.
fj_sync() {
  local dir=$1 org=$2 name=$3 branch=$4 desc=$5 errors=$6
  local porcelain=$(mktemp) refs=$(mktemp) made= summary= want= pointers=
  local divergence=0 current=0
  local -a lfs=()
  FJ_NOTE=
  FJ_PUSHED=0
  FJ_CHANGED=0
  FJ_SKIPPED=0
  FJ_LFS=0
  FJ_RETRIED=0
  FJ_ACTIONS=0
  FJ_ERROR=

  # A copy on the instance that something else writes. Leave it alone.
  if fj_skipped $org $name; then
    FJ_NOTE+=", forgejo: skipped, the copy there is the one that is maintained"
    FJ_SKIPPED=1
    rm -f -- $porcelain
    return 0
  fi

  # No branches and no tags: an empty repository upstream, with nothing to
  # refetch and nothing to push. Both callers test for that themselves and say
  # so in their own words, because it is a fact and not a failure; this is the
  # same test again, so a caller that forgets cannot create an empty repository
  # on the instance by accident.
  if ! fj_has_refs $dir; then
    rm -f -- $porcelain
    return 0
  fi

  if fj_is_partial $dir; then
    fj_stage "refetching trees and blobs"
    print -ru2 -- "    $org/$name: commits-only clone, refetching trees and blobs for the push"
    if ! fj_backfill $dir 2>> $errors; then
      FJ_ERROR="refetching the full history failed"
      rm -f -- $porcelain
      return 1
    fi
    FJ_NOTE=", refetched whole"
  fi

  fj_stage "creating the forgejo repository"
  if ! fj_ensure_repo $org $name "$desc"; then
    rm -f -- $porcelain
    return 1
  fi
  made=$FJ_ACTION

  # No mirror runs anyone's CI here; see fj_actions_off.
  fj_stage "turning the actions unit off"
  if ! fj_actions_off $org $name "$FJ_BODY"; then
    rm -f -- $porcelain
    return 1
  fi

  # Never force a push over work that is only on the instance -- and never
  # force one without having been able to ask whether there is any.
  fj_stage "reading the refs the instance already has"
  divergence=0
  fj_diverged $dir $org $name $refs $errors || divergence=$?
  if (( divergence == 0 )); then
    print -ru2 -- "    $org/$name: $FJ_DIVERGED on the instance is not in the clone; not pushing over it"
    FJ_NOTE+=", forgejo: skipped, the instance holds commits the clone does not"
    FJ_SKIPPED=1
    rm -f -- $porcelain $refs
    return 0
  elif (( divergence == 2 )); then
    # FJ_ERROR is set; git's words are already in $errors.
    rm -f -- $porcelain $refs
    return 1
  fi

  # Whether anything is going to move at all. A repository whose refs are
  # already the clone's has no object the instance could be missing, so the
  # git-lfs scan and the re-upload behind it are skipped -- that is nearly
  # every repository on nearly every run. FJ_LFS_ALWAYS=1 does the git-lfs work
  # anyway, for an instance whose objects are in doubt.
  if (( ! ${FJ_LFS_ALWAYS:-0} )) && fj_refs_match $dir $refs; then
    current=1
  fi
  rm -f -- $refs

  # The objects before the refs that point at them, so the repository on the
  # instance is never briefly one whose files cannot be checked out. None of it
  # happens unless the caller asked for git-lfs at all: the scan alone walks
  # every ref. See the git-lfs section above.
  if (( ${FJ_LFS_ENABLED:-0} )) && (( ! current )) && fj_lfs_available; then
    fj_stage "looking for git-lfs pointers"
    pointers=$(fj_lfs_files $dir)
    [[ -z $pointers ]] || lfs=(${(f)pointers})
  fi
  if (( ${#lfs} )); then
    fj_stage "fetching any of ${#lfs} git-lfs object$( (( ${#lfs} == 1 )) || print -n s) the clone lacks"
    if ! fj_lfs_fetch $dir > /dev/null 2>> $errors; then
      FJ_ERROR="fetching the ${#lfs} git-lfs object$( (( ${#lfs} == 1 )) || print -n s) from github.com failed"
      rm -f -- $porcelain
      return 1
    fi
    # git-lfs sends all of them, not the ones the instance lacks; see
    # fj_refs_match for what that costs and why this is reached as rarely as
    # possible.
    fj_stage "uploading ${#lfs} git-lfs object$( (( ${#lfs} == 1 )) || print -n s)"
    if ! fj_lfs_push $dir $org $name > /dev/null 2>> $errors; then
      FJ_ERROR="pushing the ${#lfs} git-lfs object$( (( ${#lfs} == 1 )) || print -n s) to $(fj_remote_url $org $name) failed"
      rm -f -- $porcelain
      return 1
    fi
    FJ_LFS=${#lfs}
  fi

  fj_stage "pushing every branch and tag"
  if ! fj_push $dir $org $name $porcelain $errors; then
    FJ_ERROR="push to $(fj_remote_url $org $name) failed"
    rm -f -- $porcelain
    return 1
  fi
  summary=$(fj_push_summary $porcelain)
  FJ_CHANGED=$(fj_push_changed $porcelain)
  FJ_PUSHED=1
  rm -f -- $porcelain

  fj_stage "setting the default branch"
  want=$(fj_default_branch $dir "$branch")
  if [[ -z $want ]]; then
    # Nothing to tell the instance, and worth hearing about: a mirror's HEAD is
    # whatever github.com advertises, so this is a clone with something odd in
    # it rather than a repository with no default branch.
    print -ru2 -- "    $org/$name: the clone's HEAD names no branch it has; leaving the default branch as the instance has it"
    FJ_NOTE+=", default branch left alone"
  elif ! fj_ensure_default_branch $org $name "$want"; then
    return 1
  fi

  if [[ $made == created ]]; then
    FJ_NOTE+=", forgejo repo created, $summary"
  else
    FJ_NOTE+=", forgejo $summary"
  fi
  if (( FJ_RETRIED )); then
    FJ_NOTE+=" after $FJ_RETRIED dropped connection$( (( FJ_RETRIED == 1 )) || print -n s)"
  fi
  if (( FJ_LFS )); then
    FJ_NOTE+=", $FJ_LFS lfs object$( (( FJ_LFS == 1 )) || print -n s) mirrored"
  fi
  if (( FJ_ACTIONS )); then
    FJ_NOTE+=", actions turned off"
  fi
  return 0
}

# fj_default_branch DIR [FALLBACK] -- the branch the clone's HEAD points at,
# which for a mirror is whatever GitHub advertises as its default, with the
# report's answer as a fallback. Empty when neither names a branch that is
# really there, because Forgejo rejects a default branch it cannot find.
#
# The prefix comes off here rather than from `symbolic-ref --short`, which does
# not strip it so much as shorten the ref to something unambiguous: a
# repository holding both refs/heads/main and refs/tags/main --
# hyperledger-indy/indy-vdr holds exactly that -- gets `heads/main`, and
# refs/heads/heads/main is not there, so this used to answer nothing for a
# repository whose default branch is perfectly ordinary.
fj_default_branch() {
  local dir=$1 fallback=${2:-} branch=
  branch=$(git -C $dir symbolic-ref HEAD 2>/dev/null) || branch=
  branch=${branch#refs/heads/}
  if [[ -z $branch ]] || ! git -C $dir show-ref --verify --quiet refs/heads/$branch; then
    branch=$fallback
  fi
  if [[ -n $branch ]] && ! git -C $dir show-ref --verify --quiet refs/heads/$branch; then
    branch=
  fi
  print -r -- $branch
}
