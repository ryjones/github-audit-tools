#!/usr/bin/env zsh
#
# find-clowarden-configs.zsh -- which repository holds each organization's
# CLOWarden config, as the CSV mirror-clowarden.zsh clones from.
#
#   ./find-clowarden-configs.zsh results/*.yaml
#   ./find-clowarden-configs.zsh -o reports/clowarden-watched-repos.csv results/lfdt.yaml
#
# CLOWarden acts on every repository in an organization but keeps its config in
# one of them, under one of five file names. Neither the repository nor the file
# name is fixed, so this reads the candidate repositories out of a
# gh-enterprise-repos report -- no listing call of its own -- and asks the API
# for each one's root listing.
#
# Writes `org,repo,config` with the second column the `<org>/<repo>` slug
# mirror-clowarden.zsh reads, and names on stderr every organization in the
# reports that has no config at all: those declare nothing, so nothing of
# theirs can be reconciled or counted as a maintainer.
#
# Only public, non-archived repositories are in a default report, so a private
# governance repository is invisible here; export with --visibility all to
# include one.

setopt err_exit no_unset pipe_fail

SELF=${0:A}

# Where a config is kept, in the order worth trying.
CANDIDATE_REPOS=(governance tsc clowarden .clowarden .github)
# The file names CLOWarden answers to; the parser in clowarden.rs knows the
# same list.
CONFIG_NAMES=(config.yaml config.yml teams.yaml teams.yml access-control.yaml clowarden.yaml clowarden.yml)

die() {
  print -ru2 -- "${SELF:t}: $1"
  exit ${2:-1}
}

# One repository, run by xargs in parallel: print `org<TAB>repo<TAB>config` when
# its root holds a config, and nothing when it does not.
probe_one() {
  local slug=$1
  local -a found
  found=(${(f)"$(gh api "/repos/$slug/contents" --jq '.[].name' 2>/dev/null || :)"})
  local name
  for name in $CONFIG_NAMES; do
    if (( ${found[(Ie)$name]} )); then
      printf '%s\t%s\t%s\n' ${slug%%/*} ${slug#*/} $name
      return 0
    fi
  done
  return 0
}

if [[ ${1:-} == --probe-one ]]; then
  shift
  (( $# == 1 )) || die "--probe-one takes one SLUG" 2
  probe_one $1
  exit 0
fi

usage() {
  print -r -- "usage: ${SELF:t} [-o OUT] [-j JOBS] REPORT [REPORT...]

  -o OUT     CSV to write (default: reports/clowarden-watched-repos.csv)
  -j JOBS    repositories probed at once (default: 6)
  REPORT     gh-enterprise-repos report, YAML or JSON; several are merged"
}

zparseopts -D -E -F -- o:=opt_out j:=opt_jobs h=opt_help -help=opt_help \
  || { usage >&2; exit 2 }
if (( $#opt_help )); then
  usage
  exit 0
fi
[[ ${1:-} == -- ]] && shift
(( $# >= 1 )) || { usage >&2; exit 2 }

for tool in gh yq; do
  (( $+commands[$tool] )) || die "$tool is required and was not found" 127
done

reports=($@)
for report in $reports; do
  [[ -r $report ]] || die "cannot read $report"
done

out=${opt_out[2]:-${SELF:h}/reports/clowarden-watched-repos.csv}
jobs=${opt_jobs[2]:-6}
[[ $jobs == <1-> ]] || die "-j takes a positive number, not ${(q)jobs}"

tmp=$(mktemp -d)
trap 'rm -rf -- $tmp' EXIT

# Every organization the reports cover, and every repository in them that could
# be holding a config.
for report in $reports; do
  yq -r '.organizations[]' $report >> $tmp/orgs || die "$report is not a report"
  yq -r '.repositories[] | .org + "/" + .name' $report >> $tmp/repos
done
sort -uf $tmp/orgs -o $tmp/orgs
sort -u $tmp/repos | while read -r slug; do
  (( ${CANDIDATE_REPOS[(Ie)${slug#*/}]} )) && print -r -- $slug
done > $tmp/candidates || :

total=$(grep -c . $tmp/candidates || :)
(( total )) || die "no ${(j:, :)CANDIDATE_REPOS} repository in ${(j:, :)${(@)reports:t}}"
print -ru2 -- "Probing $total candidate repositories from ${(j:, :)${(@)reports:t}}, $jobs at a time."

xargs -n 1 -P $jobs $SELF --probe-one < $tmp/candidates | sort -f > $tmp/found

# One line per config, so a run says what it found rather than going quiet.
while IFS=$'\t' read -r org repo config; do
  print -ru2 -- "  $org/$repo -- $config"
done < $tmp/found

# Written aside and renamed, so a failed run leaves the old list in place.
mkdir -p ${out:h}
{
  print -r -- 'org,repo,config'
  while IFS=$'\t' read -r org repo config; do
    printf '"%s","%s/%s","%s"\n' $org $org $repo $config
  done < $tmp/found
} > $out.$$.tmp
mv -f -- $out.$$.tmp $out

cut -f1 $tmp/found | sort -uf > $tmp/found-orgs
missing=(${(f)"$(comm -23 $tmp/orgs $tmp/found-orgs)"})
print -ru2 -- "$(grep -c . $tmp/found) configs in $(grep -c . $tmp/found-orgs) of $(grep -c . $tmp/orgs) organizations -> $out"
if (( $#missing )); then
  print -ru2 -- "No config found in ${#missing} organizations, which declare nothing:"
  print -ru2 -- "  ${(j:, :)missing}"
fi
