#!/usr/bin/env zsh
#
# github-lib.zsh -- which GitHub credential a request uses, for the scripts.
# The same rule src/token.rs applies to the Rust tools, so a run of either
# reads the same organizations.
#
# One credential serves nearly every organization. The exception is an
# organization that has turned classic personal access tokens off, which
# GitHub answers with a flat refusal rather than a thinner result:
#
#   `LFDT-Lineth` forbids access via a personal access token (classic).
#   Please use a GitHub App, OAuth App, or a personal access token with
#   fine-grained permissions.
#
# Every endpoint for it answers 403 -- organization and repository alike -- so
# such an organization needs a token of its own, named in the environment as
# GITHUB_<LOGIN>_TOKEN: the login uppercased, with anything that is not a
# letter or a digit an underscore. GITHUB_LFDT_LINETH_TOKEN serves
# LFDT-Lineth. One name, derived one way -- a second guess, such as the
# login's last segment, would have GITHUB_LABS_TOKEN quietly claim every
# organization whose name ends in -labs. A token kept under another name is
# named outright in org-tokens.txt, as `LOGIN = VARIABLE`.
#
# It also has to be *named*, in org-tokens.txt beside these scripts, because an
# enterprise listing will not name it: the GraphQL query returns a null where
# the organization should be and never its login.
#
# Sourced, never run.
#
# Environment:
#   GITHUB_ORG_TOKENS_FILE  the declarations (default org-tokens.txt beside
#                           the scripts)

typeset -g GH_LIB=${0:A:h}

# gh_org_token_vars ORG -- the variable that would hold this organization's
# token. One name; see the header.
gh_org_token_vars() {
  local login=$1 whole=
  whole=${${(U)login}//[^A-Z0-9]/_}
  whole=${${whole##_##}%%_##}
  [[ -n $whole ]] || return 0
  print -r -- "GITHUB_${whole}_TOKEN"
  return 0
}

# gh_org_token_var ORG -- the name of the variable this organization's token is
# actually in, or nothing. The name, never the value: a value printed is a
# value in a log.
gh_org_token_var() {
  local name=
  for name in $(gh_org_token_vars $1); do
    if [[ -n ${(P)name:-} ]]; then
      print -r -- $name
      return 0
    fi
  done
  return 1
}

# gh_declared_orgs [FILE] -- the organizations named in org-tokens.txt, one per
# line, each as ORG or ORG=VARIABLE.
gh_declared_orgs() {
  local file=${1:-${GITHUB_ORG_TOKENS_FILE:-$GH_LIB/org-tokens.txt}}
  [[ -r $file ]] || return 0
  local line=
  while IFS= read -r line; do
    line=${line%%\#*}
    line=${line##[[:space:]]##}
    line=${line%%[[:space:]]##}
    [[ -n $line ]] || continue
    print -r -- ${line// /}
  done < $file
  return 0
}

# gh_api PATH [ARG...] -- gh api, with the credential the path calls for.
#
# Every REST path that belongs to an organization says so in it -- orgs/<login>
# or repos/<login> -- so the right token can be chosen without any caller
# passing one. GH_TOKEN is set for that one command only, so the value never
# reaches a command line or a log.
gh_api() {
  local api_path=$1 owner= var=
  case ${api_path#/} in
    (orgs/*|repos/*)
      owner=${${api_path#/}#*/}
      owner=${owner%%/*}
      owner=${owner%%[?#]*}
      ;;
  esac
  if [[ -n $owner ]] && var=$(gh_org_token_var $owner); then
    GH_TOKEN=${(P)var} GITHUB_TOKEN=${(P)var} gh api "$@"
    return $?
  fi
  gh api "$@"
}

# gh_declared_orgs_to_add [ORG...] -- the declared organizations not already
# in the list given, and whose token is set, one per line for the caller to
# append. Says on stderr which variable each one came from, and says so too
# when a declared organization has no token, because leaving it out silently
# is the thing this whole mechanism exists to stop.
#
# It prints rather than filling an array in place: zsh's named references are
# not in every build this has to run on.
gh_declared_orgs_to_add() {
  local -a have=("$@")
  local decl= login= want= var=
  for decl in $(gh_declared_orgs); do
    login=${decl%%=*}
    want=${decl#*=}
    [[ $want == $decl ]] && want=
    (( ${have[(Ie)$login]} )) && continue
    if [[ -n $want ]]; then
      [[ -n ${(P)want:-} ]] && var=$want || var=
    else
      var=$(gh_org_token_var $login) || var=
    fi
    if [[ -n $var ]]; then
      print -ru2 -- "→ + $login (declared, token from $var)"
      print -r -- $login
    else
      print -ru2 -- "→ warning: $login is declared in ${${GITHUB_ORG_TOKENS_FILE:-org-tokens.txt}:t} but $(gh_org_token_vars $login | paste -sd'/' -) is not set; leaving it out"
    fi
  done
  return 0
}
