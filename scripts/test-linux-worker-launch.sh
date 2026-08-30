#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
test_root="$(mktemp -d)"
trap 'rm -rf -- "$test_root"' EXIT

rustc --edition=2024 \
  "$repo_root/scripts/linux-worker-launch-fixture.rs" \
  -D warnings \
  -o "$test_root/scribe-linux-worker-fixture"

rustc --edition=2024 --test \
  "$repo_root/src/linux_worker_launch.rs" \
  -D warnings -A dead-code \
  -o "$test_root/linux-worker-launch-tests"

SCRIBE_LINUX_WORKER_FIXTURE="$test_root/scribe-linux-worker-fixture" \
  "$test_root/linux-worker-launch-tests" --test-threads=1

rustc --edition=2024 --test \
  "$repo_root/src/linux_worker_architecture_guard.rs" \
  -D warnings \
  -o "$test_root/linux-worker-architecture-tests"

(
  cd "$repo_root"
  "$test_root/linux-worker-architecture-tests"
)
