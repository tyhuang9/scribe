#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$repo_root"
cargo_bin="${SCRIBE_ADMISSION_CARGO:-cargo}"
filters=(
  'gpu_worker_pack::tests::packaging_admission_'
  'gpu_worker_pack::store::tests::read_only_source_admission_'
  'architecture_guard::release_packaging_'
)

for filter in "${filters[@]}"; do
  if listing="$("$cargo_bin" test --locked --offline --bin local-transcriber "$filter" -- --list)"; then
    :
  else
    status=$?
    printf 'Worker-pack admission discovery failed for %s (exit %s).\n' "$filter" "$status" >&2
    exit "$status"
  fi

  discovered=()
  discovered_count=0
  while IFS= read -r row; do
    [[ "$row" == *': test' ]] || continue
    if [[ "$row" != "$filter"* ]]; then
      printf 'Unexpected test in worker-pack admission discovery: %s\n' "$row" >&2
      exit 1
    fi
    # Duplicate rows must not satisfy the minimum discovery requirement.
    # Bash 3.2 treats expansion of an empty array as unset under nounset.
    if [[ "$discovered_count" -gt 0 ]]; then
      for previous in "${discovered[@]}"; do
        if [[ "$row" == "$previous" ]]; then
          printf 'Duplicate worker-pack admission test: %s\n' "$row" >&2
          exit 1
        fi
      done
    fi
    discovered+=("$row")
    discovered_count=$((discovered_count + 1))
  done <<<"$listing"
  if [[ "$discovered_count" -lt 2 ]]; then
    printf 'Expected at least two worker-pack admission tests for %s; found %s.\n' "$filter" "$discovered_count" >&2
    exit 1
  fi

  printf 'Running %s discovered tests for %s\n' "$discovered_count" "$filter"
  "$cargo_bin" test --locked --offline --bin local-transcriber "$filter" -- --include-ignored --test-threads=1
done
