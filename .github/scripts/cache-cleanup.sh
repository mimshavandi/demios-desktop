#!/usr/bin/env bash
# Keep the Actions cache under the 10 GB repo limit (T-GOV-13). Deletes:
#   1. every entry saved by a pull request that is now closed or merged;
#   2. superseded Rust caches on main: for each job and OS, only the newest
#      Swatinem/rust-cache entry (v0-rust-<job>-<os>-<arch>-<hash>-<hash>) is kept.
# Other main entries (npm, tool binaries) are small and left to GitHub's 7-day expiry.
# Needs GH_TOKEN with actions: write and GH_REPO=owner/name. DRY_RUN=1 only prints.
set -euo pipefail
: "${GH_REPO:?set GH_REPO=owner/name}"

all=$(gh api --paginate "repos/$GH_REPO/actions/caches?per_page=100" --jq '.actions_caches[]' | jq -s .)

# 1. Closed pull requests.
closed=()
for pr in $(jq -r '.[].ref | capture("^refs/pull/(?<n>[0-9]+)/").n // empty' <<<"$all" | sort -un); do
  state=$(gh api "repos/$GH_REPO/pulls/$pr" --jq .state 2>/dev/null || echo unknown)
  if [ "$state" = closed ]; then closed+=("$pr"); fi
done
closed_json=$(printf '%s\n' "${closed[@]+"${closed[@]}"}" | jq -R 'select(length > 0)' | jq -s .)

# 2. Superseded Rust caches on main: newest first, keep the first of each group.
doomed=$(jq -r --argjson closed "$closed_json" '
  ( map(select(.ref | test("^refs/pull/")))
    | map(select((.ref | capture("^refs/pull/(?<n>[0-9]+)/").n) as $n | $closed | index($n)))
    | map(. + {why: "closed PR"}) )
  + ( map(select(.ref == "refs/heads/main" and (.key | startswith("v0-rust-"))))
      | group_by(.key | sub("-[0-9a-f]+-[0-9a-f]+$"; ""))
      | map(sort_by(.created_at) | reverse | .[1:][])
      | map(. + {why: "superseded on main"}) )
  | .[] | [.id, .size_in_bytes, .ref, .key, .why] | @tsv' <<<"$all")

freed=0
while IFS=$'\t' read -r id size ref key why; do
  [ -n "$id" ] || continue
  printf 'delete %-10s %6d MB  %-20s %s (%s)\n' "$id" $((size / 1000000)) "$ref" "$key" "$why"
  freed=$((freed + size))
  if [ "${DRY_RUN:-}" != 1 ]; then
    gh api -X DELETE "repos/$GH_REPO/actions/caches/$id" --silent || echo "  (already gone)"
  fi
done <<<"$doomed"

total=$(jq '[.[].size_in_bytes] | add // 0' <<<"$all")
echo "caches: $(jq length <<<"$all"), $((total / 1000000)) MB; freed $((freed / 1000000)) MB"
