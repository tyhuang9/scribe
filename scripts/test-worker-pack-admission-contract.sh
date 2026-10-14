#!/usr/bin/env bash
set -euo pipefail

filters=(
  'gpu_worker_pack::tests::packaging_admission_'
  'gpu_worker_pack::store::tests::read_only_source_admission_'
  'architecture_guard::release_packaging_'
)

# A copy of this script supplies a fake Cargo executable. It validates exact
# arguments and never invokes Cargo, native tools, a worker, or the network.
if [[ "${SCRIBE_ADMISSION_CONTRACT_FAKE:-0}" == 1 ]]; then
  [[ "$#" -ge 8 && "$1" == test && "$2" == --locked && "$3" == --offline && "$4" == --bin && "$5" == local-transcriber && "$7" == -- ]] || exit 90
  group=0
  for index in 0 1 2; do
    if [[ "$6" == "${filters[$index]}" ]]; then group=$((index + 1)); fi
  done
  [[ "$group" -ne 0 ]] || exit 91
  case "$8" in
    --list) [[ "$#" -eq 8 ]] || exit 92; stage=list ;;
    --include-ignored) [[ "$#" -eq 9 && "$9" == --test-threads=1 ]] || exit 92; stage=run ;;
    *) exit 92 ;;
  esac
  printf '%s:%s\n' "$6" "$stage" >>"${SCRIBE_ADMISSION_CONTRACT_TRACE:?}"
  scenario="${SCRIBE_ADMISSION_CONTRACT_SCENARIO:?}"
  if [[ "$stage" == run ]]; then
    [[ "$scenario" != "run-failure-$group" ]] || exit 32
    exit 0
  fi
  [[ "$scenario" != "list-failure-$group" ]] || exit 31
  if [[ "$group" == 1 ]]; then
    case "$scenario" in
      empty) printf '0 tests, 0 benchmarks\n'; exit 0 ;;
      one) printf '%sfixture_one: test\n' "$6"; exit 0 ;;
      wrong-filter) printf 'unrelated::fixture: test\n'; exit 0 ;;
      duplicate) printf '%sfixture_one: test\n%sfixture_one: test\n' "$6" "$6"; exit 0 ;;
      benchmark-only) printf '%sfixture_one: benchmark\n%sfixture_two: benchmark\n' "$6" "$6"; exit 0 ;;
    esac
  fi
  printf 'Test discovery heading\n%sfixture_one: test\n%sfixture_two: test\n\n2 tests, 0 benchmarks\n' "$6" "$6"
  exit 0
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
runner="$repo_root/scripts/test-worker-pack-admission.sh"
bash -n "$runner"
bash -n "${BASH_SOURCE[0]}"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/scribe-admission-contract.XXXXXX")"
cleanup() {
  rm -f -- "$scratch/mock cargo" "$scratch/trace" "$scratch/stdout" "$scratch/stderr"
  rmdir -- "$scratch"
}
trap cleanup EXIT
cp "${BASH_SOURCE[0]}" "$scratch/mock cargo"
chmod +x "$scratch/mock cargo"
expected_trace=()
for filter in "${filters[@]}"; do
  expected_trace+=("$filter:list" "$filter:run")
done

assert_case() {
  local scenario="$1" expected_status="$2" expected_count="$3" status actual expected
  : >"$scratch/trace"
  if SCRIBE_ADMISSION_CARGO="$scratch/mock cargo" \
    SCRIBE_ADMISSION_CONTRACT_FAKE=1 \
    SCRIBE_ADMISSION_CONTRACT_TRACE="$scratch/trace" \
    SCRIBE_ADMISSION_CONTRACT_SCENARIO="$scenario" \
    bash "$runner" >"$scratch/stdout" 2>"$scratch/stderr"; then
    status=0
  else
    status=$?
  fi
  actual="$(<"$scratch/trace")"
  expected="$(printf '%s\n' "${expected_trace[@]:0:$expected_count}")"
  if [[ "$status" != "$expected_status" || "$actual" != "$expected" ]]; then
    printf 'Worker-pack admission contract failed: %s (exit %s, expected %s; calls must stop at %s).\n' "$scenario" "$status" "$expected_status" "$expected_count" >&2
    exit 1
  fi
  printf 'PASS %s\n' "$scenario"
}

assert_case success 0 6
assert_case empty 1 1
assert_case one 1 1
assert_case wrong-filter 1 1
assert_case duplicate 1 1
assert_case benchmark-only 1 1
assert_case list-failure-1 31 1
assert_case list-failure-2 31 3
assert_case list-failure-3 31 5
assert_case run-failure-1 32 2
assert_case run-failure-2 32 4
assert_case run-failure-3 32 6

# The canonical command belongs only in the existing secret-free structural
# matrix, after native test preparation, never in the protected signing job.
workflow="$repo_root/.github/workflows/macos-release.yml"
command='bash scripts/test-worker-pack-admission.sh'
[[ "$(grep -Fc "$command" "$workflow")" == 1 ]]
structural="$(sed -n '/^  structural:/,/^  official-sign-notarize:/p' "$workflow")"
[[ "$(grep -Fc "$command" <<<"$structural")" == 1 ]]
! grep -F 'secrets.' <<<"$structural" >/dev/null
for script in test-worker-pack-admission.sh test-worker-pack-admission-contract.sh; do
  [[ "$(grep -Fc "      - 'scripts/$script'" "$workflow")" == 2 ]]
done
printf 'PASS macOS structural workflow wiring\n'
