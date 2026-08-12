#!/usr/bin/env zsh
#
# gh-enterprise-ghsa.zsh — list every GitHub Security Advisory (GHSA) across the
# organizations in a GitHub enterprise, along with any CVE assigned to it.
#
# There is no enterprise-level advisory REST endpoint, so this expands the
# enterprise into its member orgs via GraphQL and then walks
# GET /orgs/{org}/security-advisories for each one.
#
# With -R it also reports each repo's advisory posture. Note that GitHub has no
# per-repo "security advisories: off" switch — any repo admin can always open a
# draft advisory. The setting that can be off is private vulnerability reporting
# (PVR): with PVR disabled, outside researchers have no way to report privately,
# so the repo only ever gets advisories its own admins write. That is what -R
# reports, via GET /repos/{owner}/{repo}/private-vulnerability-reporting.
#
# Requires: gh (authenticated), jq
# Token scopes: read:enterprise (to expand the enterprise) plus admin/security-manager
#               rights on each org (to read its advisories). -R additionally needs
#               admin on each repo; repos without it are reported as "unknown".
#
# Usage:
#   ./gh-enterprise-ghsa.zsh                                # default enterprise
#   ./gh-enterprise-ghsa.zsh -e my-enterprise -s published
#   ./gh-enterprise-ghsa.zsh -f csv > advisories.csv
#   ./gh-enterprise-ghsa.zsh -f json | jq '.[] | select(.cve_id)'
#   ./gh-enterprise-ghsa.zsh -o my-org -o my-org
#   ./gh-enterprise-ghsa.zsh -c                             # only CVE-assigned
#   ./gh-enterprise-ghsa.zsh -R                             # + repo PVR posture
#   ./gh-enterprise-ghsa.zsh -RD                            # PVR-disabled repos only
#   ./gh-enterprise-ghsa.zsh -RA                            # skip the advisory sweep
#   ./gh-enterprise-ghsa.zsh -R -f md -O reports/pqca.md    # markdown to a file

set -euo pipefail

ENTERPRISE="post-quantum-cryptography-alliance"
STATE="all"          # all | triage | draft | published | closed
FORMAT="table"       # table | csv | tsv | json | ghsa
CVE_ONLY=0
REPO_POSTURE=0       # -R: also report per-repo private vulnerability reporting
DISABLED_ONLY=0      # -D: in the repo section, list only PVR-disabled repos
NO_ADVISORIES=0      # -A: skip the advisory sweep entirely
INCLUDE_ARCHIVED=0   # -a: include archived repos in the repo section
PVR_JOBS=8           # parallel PVR probes
OUTFILE=""           # -O: write the report here instead of stdout
FORCE=0              # -F: allow -O to overwrite an existing file
typeset -a ORGS
ORGS=()

SCRIPT_NAME=${0:t}   # capture now: inside a zsh function, $0 is the function name

usage() {
  cat >&2 <<EOF
Usage: $SCRIPT_NAME [options]

  -e SLUG    enterprise slug (default: $ENTERPRISE)
  -o ORG     only this org; repeatable. Skips enterprise expansion.
  -s STATE   all | triage | draft | published | closed (default: all)
  -f FORMAT  table | md | csv | tsv | json | ghsa (default: table)
  -c         only advisories that have a CVE assigned
  -R         also report per-repo private vulnerability reporting (PVR) posture
  -D         with -R, list only repos where PVR is disabled
  -A         skip the advisory sweep (use with -R for a posture-only report)
  -a         with -R, include archived repos (excluded by default)
  -j N       parallel PVR probes (default: $PVR_JOBS)
  -O FILE    write the report to FILE instead of stdout ("-" means stdout)
  -F         allow -O to overwrite an existing file
  -h         this help
EOF
  exit "${1:-0}"
}

while getopts ":e:o:s:f:j:O:cRDAaFh" opt; do
  case $opt in
    e) ENTERPRISE=$OPTARG ;;
    o) ORGS+=("$OPTARG") ;;
    s) STATE=$OPTARG ;;
    f) FORMAT=$OPTARG ;;
    j) PVR_JOBS=$OPTARG ;;
    c) CVE_ONLY=1 ;;
    R) REPO_POSTURE=1 ;;
    D) DISABLED_ONLY=1 ;;
    A) NO_ADVISORIES=1 ;;
    a) INCLUDE_ARCHIVED=1 ;;
    O) OUTFILE=$OPTARG ;;
    F) FORCE=1 ;;
    h) usage 0 ;;
    :) print -u2 "error: -$OPTARG requires an argument"; usage 1 ;;
    \?) print -u2 "error: unknown option -$OPTARG"; usage 1 ;;
  esac
done

case $STATE in
  all|triage|draft|published|closed) ;;
  *) print -u2 "error: bad -s state: $STATE"; usage 1 ;;
esac
case $FORMAT in
  table|csv|tsv|json|ghsa|md) ;;
  *) print -u2 "error: bad -f format: $FORMAT"; usage 1 ;;
esac
[[ $PVR_JOBS == <-> ]] && (( PVR_JOBS >= 1 )) || { print -u2 "error: -j wants a positive integer"; usage 1 }
(( NO_ADVISORIES && ! REPO_POSTURE )) && { print -u2 "error: -A leaves nothing to do without -R"; usage 1 }
(( DISABLED_ONLY && ! REPO_POSTURE )) && { print -u2 "error: -D only applies with -R"; usage 1 }
if [[ $FORMAT == ghsa ]] && (( REPO_POSTURE )); then
  print -u2 "note: -f ghsa emits GHSA/CVE pairs only; the -R section is suppressed"
fi

# Validate -O up front so we fail before spending minutes on API calls.
if [[ -n $OUTFILE && $OUTFILE != - ]]; then
  if [[ -e $OUTFILE ]] && (( ! FORCE )); then
    print -u2 "error: $OUTFILE exists (pass -F to overwrite)"; exit 1
  fi
  typeset outdir=${OUTFILE:h}
  [[ -d $outdir ]] || { print -u2 "error: no such directory: $outdir"; exit 1 }
  [[ -w $outdir ]] || { print -u2 "error: directory not writable: $outdir"; exit 1 }
fi

command -v gh >/dev/null || { print -u2 "error: gh not found"; exit 127 }
command -v jq >/dev/null || { print -u2 "error: jq not found"; exit 127 }
gh auth status >/dev/null 2>&1 || { print -u2 "error: gh is not authenticated (run: gh auth login)"; exit 1 }

# --- resolve the enterprise into its member orgs ------------------------------
if (( ${#ORGS} == 0 )); then
  print -u2 "→ expanding enterprise '$ENTERPRISE' …"
  typeset gql_out
  if ! gql_out=$(gh api graphql -F slug="$ENTERPRISE" -f query='
      query($slug:String!, $cursor:String) {
        enterprise(slug:$slug) {
          name
          organizations(first:100, after:$cursor) {
            nodes { login }
            pageInfo { hasNextPage endCursor }
          }
        }
      }' --paginate --slurp 2>&1); then
    print -u2 "error: could not read enterprise '$ENTERPRISE'"
    print -u2 "$gql_out"
    print -u2 "hint: needs the read:enterprise scope — gh auth refresh -h github.com -s read:enterprise"
    exit 1
  fi

  typeset ent_name
  ent_name=$(print -r -- "$gql_out" | jq -r '[.[].data.enterprise.name]|map(select(.))|first // empty')
  [[ -n $ent_name ]] || { print -u2 "error: enterprise '$ENTERPRISE' not found or not visible to this token"; exit 1 }

  ORGS=(${(f)"$(print -r -- "$gql_out" | jq -r '[.[].data.enterprise.organizations.nodes[].login]|unique|.[]')"})
  print -u2 "→ $ent_name: ${#ORGS} org(s): ${ORGS[*]}"
fi

(( ${#ORGS} > 0 )) || { print -u2 "error: no organizations to query"; exit 1 }

# --- collect advisories -------------------------------------------------------
# The org listing returns .repository as null, so the repo is recovered from html_url.
typeset -a json_parts
typeset org api_path raw norm count
typeset failed=0

(( NO_ADVISORIES )) && ORGS_FOR_ADVISORIES=() || ORGS_FOR_ADVISORIES=($ORGS)

for org in $ORGS_FOR_ADVISORIES; do
  # NB: do not name this variable `path` — in zsh that is tied to $PATH.
  api_path="/orgs/$org/security-advisories?per_page=100&sort=created&direction=desc"
  [[ $STATE != all ]] && api_path+="&state=$STATE"

  # A 403/404 here just means this token isn't an owner/security manager of $org.
  # Warn and keep going so one inaccessible org doesn't sink the whole sweep.
  if ! raw=$(gh api --paginate --slurp "$api_path" 2>&1); then
    # gh appends its own "gh: ..." line to the body, so $raw isn't valid JSON — scrape it.
    typeset why
    why=$(print -r -- "$raw" | sed -n 's/.*"message":"\([^"]*\)".*/\1/p' | head -1)
    print -u2 "! skipping $org: ${why:-${raw//$'\n'/ }} (no advisory read access?)"
    failed=1
    continue
  fi

  norm=$(print -r -- "$raw" | jq -c --arg org "$org" '
    (if length > 0 and (.[0]|type) == "array" then add else . end) // []
    | map({
        org:        $org,
        repo:       (.repository.full_name
                     // (.html_url | capture("github\\.com/(?<r>[^/]+/[^/]+)/security") | .r)
                     // null),
        ghsa_id:    .ghsa_id,
        cve_id:     .cve_id,
        # GitHub auto-creates a private fork per draft advisory; -R filters these out.
        private_fork: (.private_fork.full_name // null),
        severity:   (.severity // "unknown"),
        state:      .state,
        # NB: "" | split("T") is [], so .[0] would be null — hence the second //.
        published:  ((.published_at // "") | split("T") | .[0] // ""),
        updated:    ((.updated_at   // "") | split("T") | .[0] // ""),
        cwe_ids:    (.cwe_ids // []),
        cvss_score: (.cvss.score // null),
        summary:    (.summary // ""),
        html_url:   .html_url,
        ecosystems: ([.vulnerabilities[]?.package.ecosystem] | unique),
        packages:   ([.vulnerabilities[]?.package.name] | unique)
      })')

  count=$(print -r -- "$norm" | jq 'length')
  print -u2 "  $org: $count advisor$( (( count == 1 )) && print -n y || print -n ies)"
  json_parts+=("$norm")
done

typeset all_json
if (( ${#json_parts} )); then
  all_json=$(printf '%s\n' "${json_parts[@]}" | jq -s -c 'add // []')
else
  all_json='[]'
fi
(( CVE_ONLY )) && all_json=$(print -r -- "$all_json" | jq -c 'map(select(.cve_id != null))')

# --- collect per-repo advisory posture (-R) -----------------------------------
# GitHub has no "advisories off" repo switch; the closest real setting is private
# vulnerability reporting. PVR off => outside researchers cannot report privately.
# The probe needs repo admin, so anything we can't read is recorded as "unknown".
typeset repos_json='[]' repos_json_all='[]'
if (( REPO_POSTURE )); then
  typeset tmpd repo_list nrepos
  tmpd=$(mktemp -d "${TMPDIR:-/tmp}/ghsa-pvr.XXXXXX")
  trap 'rm -rf -- "$tmpd"' EXIT INT TERM

  # 1. enumerate repos across the orgs
  repo_list=""
  for org in $ORGS; do
    if ! raw=$(gh api --paginate --slurp "/orgs/$org/repos?per_page=100&type=all" 2>&1); then
      why=$(print -r -- "$raw" | sed -n 's/.*"message":"\([^"]*\)".*/\1/p' | head -1)
      print -u2 "! skipping repos of $org: ${why:-unreadable}"
      failed=1
      continue
    fi
    repo_list+=$(print -r -- "$raw" | jq -r --argjson keep_arch "$INCLUDE_ARCHIVED" '
      (if length > 0 and (.[0]|type) == "array" then add else . end) // []
      | map(select($keep_arch == 1 or .archived == false))
      | .[] | [.full_name, (.archived|tostring), (.fork|tostring), (.visibility // "unknown")]
      | @tsv')
    repo_list+=$'\n'
  done
  repo_list=${repo_list//$'\n'$'\n'/$'\n'}

  nrepos=$(print -r -- "$repo_list" | grep -c . || true)
  print -u2 "→ probing private vulnerability reporting on $nrepos repo(s), ${PVR_JOBS} at a time …"

  # 2. probe PVR, PVR_JOBS at a time (one REST call per repo; no bulk endpoint exists)
  typeset -i n=0
  typeset full arch frk vis
  while IFS=$'\t' read -r full arch frk vis; do
    [[ -n $full ]] || continue
    (( ++n ))
    (
      typeset pvr_raw enabled
      if [[ $vis != public ]]; then
        # PVR is a public-repo feature; the endpoint 404s on private repos even
        # for admins, so don't spend a call and don't call it "unknown".
        enabled="n/a"
      elif pvr_raw=$(gh api "/repos/$full/private-vulnerability-reporting" 2>/dev/null); then
        enabled=$(print -r -- "$pvr_raw" | jq -r 'if .enabled == true then "enabled" elif .enabled == false then "disabled" else "unknown" end')
      else
        enabled="unknown"   # usually: not a repo admin
      fi
      jq -n -c --arg repo "$full" --arg pvr "$enabled" --arg arch "$arch" \
              --arg fork "$frk" --arg vis "$vis" '{
        org: ($repo|split("/")[0]), repo: $repo, name: ($repo|split("/")[1]),
        pvr: $pvr, archived: ($arch == "true"), fork: ($fork == "true"), visibility: $vis
      }'
    ) > "$tmpd/$n.json" &
    (( n % PVR_JOBS == 0 )) && { wait || true }
  done <<< "$repo_list"
  wait || true

  repos_json=$(cat "$tmpd"/*.json 2>/dev/null | jq -s -c 'sort_by(.org, .name)' || print -n '[]')

  # Fold in advisory counts so a PVR-disabled repo that still has advisories
  # stands out, and drop GitHub's per-advisory private forks — they are an
  # artifact of the advisory workflow, not repos anyone configures.
  repos_json=$(jq -n -c --argjson repos "$repos_json" --argjson advs "$all_json" '
    ($advs | group_by(.repo) | map({key: (.[0].repo // "?"), value: length}) | from_entries) as $counts
    | ($advs | map(.private_fork) | map(select(.)) | unique) as $forks
    | $repos
    | map(select(.repo as $r | $forks | index($r) | not))
    | map(. + {advisories: ($counts[.repo] // 0)})')

  # Keep the unfiltered set so -D narrows the listing without skewing the totals.
  repos_json_all=$repos_json
  (( DISABLED_ONLY )) && repos_json=$(print -r -- "$repos_json" | jq -c 'map(select(.pvr == "disabled"))')
fi

# --- render -------------------------------------------------------------------
# Everything here writes to stdout, so -O can capture the whole report with one
# redirect. Progress and warnings stay on stderr regardless.
render_advisories() {
  case $FORMAT in
    ghsa)
      print -r -- "$all_json" | jq -r '.[] | "\(.ghsa_id)\t\(.cve_id // "-")"'
      ;;

    csv)
      print -r -- "$all_json" | jq -r '
        (["org","repo","ghsa_id","cve_id","severity","cvss","state","published","cwe_ids","packages","summary","url"]),
        (.[] | [ .org, (.repo // ""), .ghsa_id, (.cve_id // ""), .severity,
                 (.cvss_score // ""|tostring), .state, .published,
                 (.cwe_ids|join(" ")), (.packages|join(" ")), .summary, .html_url ])
        | @csv'
      ;;

    tsv)
      print -r -- "$all_json" | jq -r '
        (["ORG","REPO","GHSA","CVE","SEVERITY","STATE","PUBLISHED","SUMMARY"]),
        (.[] | [ .org, (.repo // "-"), .ghsa_id, (.cve_id // "-"),
                 (.severity|ascii_upcase), .state, (if .published == "" then "-" else .published end),
                 .summary ])
        | @tsv'
      ;;

    md)
      print -r -- "$all_json" | jq -r '
        # escape pipes so a summary containing "|" cannot break the table
        def cell: (. // "-") | tostring | gsub("\\|"; "\\\\|") | gsub("\n"; " ");
        "## Security advisories",
        "",
        "| Org | Repo | GHSA | CVE | Severity | State | Published | Summary |",
        "| --- | --- | --- | --- | --- | --- | --- | --- |",
        (.[] | "| \(.org|cell) | \((.repo // "-")|sub("^[^/]+/";"")|cell) " +
               "| [\(.ghsa_id)](\(.html_url)) | \(.cve_id|cell) | \(.severity|cell) " +
               "| \(.state|cell) | \(if .published == "" then "-" else .published end|cell) " +
               "| \(.summary|cell) |")'
      ;;

    table)
      print -r -- "$all_json" | jq -r '
        def trunc($n): if (.|length) > $n then (.[0:$n-1] + "…") else . end;
        (["ORG","REPO","GHSA","CVE","SEVERITY","STATE","PUBLISHED","SUMMARY"]),
        (.[] | [ .org,
                 ((.repo // "-") | sub("^[^/]+/";"")),
                 .ghsa_id,
                 (.cve_id // "-"),
                 (.severity|ascii_upcase),
                 .state,
                 (if .published == "" then "-" else .published end),
                 (.summary | trunc(64)) ])
        | @tsv' | column -t -s $'\t'
      ;;
  esac
}

render_repos() {
  case $FORMAT in
    csv)
      print -r -- "$repos_json" | jq -r '
        (["org","repo","pvr","advisories","visibility","archived","fork"]),
        (.[] | [ .org, .repo, .pvr, (.advisories|tostring), .visibility,
                 (.archived|tostring), (.fork|tostring) ])
        | @csv'
      ;;

    tsv)
      print -r -- "$repos_json" | jq -r '
        (["ORG","REPO","PVR","ADVISORIES","VISIBILITY","ARCHIVED","FORK"]),
        (.[] | [ .org, .repo, (.pvr|ascii_upcase), (.advisories|tostring), .visibility,
                 (.archived|tostring), (.fork|tostring) ])
        | @tsv'
      ;;

    md)
      print -r -- "$repos_json" | jq -r '
        "## Private vulnerability reporting",
        "",
        "| Org | Repo | PVR | Advisories | Visibility | Archived |",
        "| --- | --- | --- | --- | --- | --- |",
        (.[] | "| \(.org) | [\(.name)](https://github.com/\(.repo)) " +
               "| \(if .pvr == "disabled" then "**disabled**" elif .pvr == "enabled" then "enabled" elif .pvr == "n/a" then "n/a _(private)_" else "unknown" end) " +
               "| \(if .advisories == 0 then "–" else (.advisories|tostring) end) " +
               "| \(.visibility) | \(if .archived then "yes" else "–" end) |")'
      ;;

    table)
      print -r -- "$repos_json" | jq -r '
        (["ORG","REPO","PVR","ADVISORIES","VISIBILITY","ARCHIVED"]),
        (.[] | [ .org, .name,
                 (if .pvr == "disabled" then "DISABLED ✗"
                  elif .pvr == "enabled" then "enabled ✓"
                  elif .pvr == "n/a" then "n/a (private)"
                  else "unknown ?" end),
                 (if .advisories == 0 then "-" else (.advisories|tostring) end),
                 .visibility,
                 (if .archived then "yes" else "-" end) ])
        | @tsv' | column -t -s $'\t'
      ;;
  esac
}

emit_report() {
if [[ $FORMAT == json ]]; then
  # -R changes the shape to an object; without it the output stays a bare array.
  if (( REPO_POSTURE )); then
    jq -n --argjson advs "$all_json" --argjson repos "$repos_json" --argjson skip "$NO_ADVISORIES" \
      'if $skip == 1 then {repos: $repos} else {advisories: $advs, repos: $repos} end'
  else
    print -r -- "$all_json" | jq '.'
  fi
else
  if (( ! NO_ADVISORIES )); then
    render_advisories

    # Totals belong to the human report, so they go to stdout for table format
    # (and thus into -O); machine formats keep stdout clean and get them on stderr.
    typeset totals_fd=2
    [[ $FORMAT == table || $FORMAT == md ]] && totals_fd=1

    if [[ $FORMAT == md ]]; then
      print -r -- "$all_json" | jq -r '
        "",
        "**Totals** — \(length) advisories, \([.[]|select(.cve_id)]|length) with a CVE, \([.[]|select(.cve_id|not)]|length) without.",
        "",
        "- by state: \(reduce .[] as $a ({}; .[$a.state] += 1) | to_entries | map("\(.key) \(.value)") | join(", ") | if . == "" then "–" else . end)",
        "- by severity: \(reduce .[] as $a ({}; .[$a.severity] += 1) | to_entries | map("\(.key) \(.value)") | join(", ") | if . == "" then "–" else . end)",
        "- CVEs: \([.[].cve_id|select(.)]|unique|map("`\(.)`")|join(", ") | if . == "" then "–" else . end)"'
    elif [[ $FORMAT == table || $FORMAT == tsv ]]; then
      print -r -- "$all_json" | jq -r '
        "\n── advisory totals ────────────────────────────────",
        "advisories       : \(length)",
        "with CVE         : \([.[]|select(.cve_id)]|length)",
        "without CVE      : \([.[]|select(.cve_id|not)]|length)",
        "by state         : \(reduce .[] as $a ({}; .[$a.state] += 1) | to_entries | map("\(.key)=\(.value)") | join(" ") | if . == "" then "-" else . end)",
        "by severity      : \(reduce .[] as $a ({}; .[$a.severity] += 1) | to_entries | map("\(.key)=\(.value)") | join(" ") | if . == "" then "-" else . end)",
        "CVEs             : \([.[].cve_id|select(.)]|unique|join(", ") | if . == "" then "-" else . end)"' >&$totals_fd
    fi
  fi

  if (( REPO_POSTURE )) && [[ $FORMAT != ghsa ]]; then
    (( ! NO_ADVISORIES )) && print ""
    render_repos

    typeset totals_fd=2
    [[ $FORMAT == table || $FORMAT == md ]] && totals_fd=1

    if [[ $FORMAT == md ]]; then
      print -r -- "$repos_json_all" | jq -r '
        "",
        "**PVR posture** — of \(length) repos checked: \([.[]|select(.pvr=="enabled")]|length) enabled, **\([.[]|select(.pvr=="disabled")]|length) disabled**, \([.[]|select(.pvr=="n/a")]|length) n/a (private), \([.[]|select(.pvr=="unknown")]|length) unknown (no admin).",
        "",
        "- Disabled: \([.[]|select(.pvr=="disabled")|.repo]|map("`\(.)`")|join(", ") | if . == "" then "_(none)_" else . end)",
        "- Disabled **and** carrying advisories: \([.[]|select(.pvr=="disabled" and .advisories>0)|"`\(.repo)` (\(.advisories))"]|join(", ") | if . == "" then "_(none)_" else . end)",
        "",
        "> PVR off means outside researchers cannot privately report a vulnerability;",
        "> repo admins can still open draft advisories themselves."'
    elif [[ $FORMAT == table || $FORMAT == tsv ]]; then
      print -r -- "$repos_json_all" | jq -r '
        "\n── private vulnerability reporting ────────────────",
        "repos checked    : \(length)",
        "PVR enabled      : \([.[]|select(.pvr=="enabled")]|length)",
        "PVR DISABLED     : \([.[]|select(.pvr=="disabled")]|length)",
        "n/a (private)    : \([.[]|select(.pvr=="n/a")]|length)",
        "unknown (no admin): \([.[]|select(.pvr=="unknown")]|length)",
        "disabled repos   : \([.[]|select(.pvr=="disabled")|.repo]|join(", ") | if . == "" then "(none)" else . end)",
        "  …of those, with advisories anyway: \([.[]|select(.pvr=="disabled" and .advisories>0)|"\(.repo) (\(.advisories))"]|join(", ") | if . == "" then "(none)" else . end)"' >&$totals_fd
    fi
  fi
fi
}

if [[ -n $OUTFILE && $OUTFILE != - ]]; then
  # Build in a sibling temp file and rename, so a failure part-way through can't
  # leave a truncated report where a good one used to be.
  typeset staged="$OUTFILE.partial.$$"
  emit_report > "$staged"
  mv -f -- "$staged" "$OUTFILE"
  print -u2 "→ wrote $FORMAT report to $OUTFILE"
else
  emit_report
fi

exit $failed
