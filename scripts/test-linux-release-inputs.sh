#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

[[ "$(uname -s)" == Linux && "$(uname -m)" == x86_64 ]] || { echo 'Linux x86_64 is required.' >&2; exit 1; }
: "${RUNNER_TEMP:?RUNNER_TEMP must identify the private temporary root.}"
[[ -d "$RUNNER_TEMP" && ! -L "$RUNNER_TEMP" ]] || { echo 'RUNNER_TEMP must be a real directory.' >&2; exit 1; }
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
runner_root="$(cd "$RUNNER_TEMP" && pwd -P)"
umask 077
test_root="$(mktemp -d "$runner_root/scribe-linux-input-tests.XXXXXX")"
cleanup() {
  local status=$?
  trap - EXIT
  [[ "$test_root" == "$runner_root"/scribe-linux-input-tests.* && ! -L "$test_root" ]] || exit 1
  rm -rf -- "$test_root" || status=1
  exit "$status"
}
trap cleanup EXIT
mkdir "$test_root/bin" "$test_root/output"
# Only the transport is replaced. Real size/hash/publication code must reject
# these inputs; no fake success is treated as native/archive evidence.
cat >"$test_root/bin/curl" <<'MOCK_CURL'
#!/usr/bin/env bash
set -euo pipefail
case "${SCRIBE_TEST_CURL_MODE:?}" in
  short) printf 'short archive' ;;
  oversized) head -c 22398150 /dev/zero ;;
  wrong-digest) head -c 22394054 /dev/zero ;;
  http-error) printf 'partial'; exit 22 ;;
  *) echo 'unexpected mock transport invocation' >&2; exit 99 ;;
esac
MOCK_CURL
chmod 0700 "$test_root/bin/curl"
expect_failure() {
  local label="$1"
  shift
  if timeout --signal=TERM --kill-after=5s 30s "$@" >"$test_root/failure.log" 2>&1; then
    echo "release input rejection unexpectedly passed: $label" >&2
    exit 1
  else
    failure_status=$?
  fi
  [[ "$failure_status" != 124 && "$failure_status" != 137 ]] || { echo 'release input rejection timed out.' >&2; exit 1; }
  shopt -s nullglob
  local staging=("$test_root/output/".scribe-linux-inputs.*)
  [[ ${#staging[@]} == 0 ]] || { echo 'failed preparation leaked staging output.' >&2; exit 1; }
}
for mode in short oversized wrong-digest http-error; do
  expect_failure "$mode" env PATH="$test_root/bin:$PATH" SCRIBE_TEST_CURL_MODE="$mode" \
    bash "$repo_root/scripts/prepare-linux-release-inputs.sh" --output-directory "$test_root/output/$mode"
  [[ ! -e "$test_root/output/$mode" && ! -L "$test_root/output/$mode" ]] || { echo 'invalid archive was published.' >&2; exit 1; }
  case "$mode" in
    short) grep -Fq 'archive size mismatch' "$test_root/failure.log" ;;
    wrong-digest) grep -Fq 'archive digest mismatch' "$test_root/failure.log" ;;
    http-error) [[ "$failure_status" == 22 ]] ;;
  esac
done
mkdir "$test_root/output/existing"
printf 'retained' >"$test_root/output/existing/sentinel"
expect_failure existing-output env PATH="$test_root/bin:$PATH" SCRIBE_TEST_CURL_MODE=unexpected \
  bash "$repo_root/scripts/prepare-linux-release-inputs.sh" --output-directory "$test_root/output/existing"
[[ "$(<"$test_root/output/existing/sentinel")" == retained ]]
ln -s existing "$test_root/output/linked"
expect_failure linked-output env PATH="$test_root/bin:$PATH" SCRIBE_TEST_CURL_MODE=unexpected \
  bash "$repo_root/scripts/prepare-linux-release-inputs.sh" --output-directory "$test_root/output/linked"
[[ -L "$test_root/output/linked" && "$(<"$test_root/output/existing/sentinel")" == retained ]]
grep -Fq 'output already exists' "$test_root/failure.log"
echo 'Linux release input failure cases passed (short, oversized, wrong digest, HTTP failure, existing/linked output).'
