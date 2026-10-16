#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
test_root="$(mktemp -d)"
trap 'rm -rf -- "$test_root"' EXIT

require_test_discovery() {
  local binary="$1"
  local expected="$2"
  local listing
  listing="$("$binary" --list)"
  if ! grep -Fqx -- "$expected: test" <<<"$listing"; then
    echo "Expected test was not discovered: $expected" >&2
    exit 1
  fi
}

require_exact_test_set() {
  local binary="$1"
  shift
  local expected
  local discovered
  expected="$(printf '%s\n' "$@" | LC_ALL=C sort)"
  discovered="$("$binary" --list | sed -n 's/: test$//p' | LC_ALL=C sort)"
  if [[ "$discovered" != "$expected" ]]; then
    echo "Discovered test set did not match the canonical inventory for $binary" >&2
    diff -u <(printf '%s\n' "$expected") <(printf '%s\n' "$discovered") >&2 || true
    exit 1
  fi
}

if [[ -r /proc/sys/vm/memfd_noexec ]]; then
  memfd_noexec="$(< /proc/sys/vm/memfd_noexec)"
  if [[ "$memfd_noexec" != "0" ]]; then
    echo "Linux worker launch tests require vm.memfd_noexec=0; found $memfd_noexec" >&2
    exit 1
  fi
fi

rustc --edition=2024 \
  "$repo_root/scripts/linux-worker-launch-fixture.rs" \
  -D warnings \
  -o "$test_root/scribe-linux-worker-fixture"

rustc --edition=2024 --test \
  "$repo_root/src/linux_worker_launch.rs" \
  -D warnings -A dead-code \
  -o "$test_root/linux-worker-launch-tests"

require_test_discovery "$test_root/linux-worker-launch-tests" "tests::sha256_matches_known_vector"
SCRIBE_LINUX_WORKER_FIXTURE="$test_root/scribe-linux-worker-fixture" \
  "$test_root/linux-worker-launch-tests" --test-threads=1

rustc --edition=2024 --test \
  "$repo_root/src/linux_worker_architecture_guard.rs" \
  -D warnings \
  -o "$test_root/linux-worker-architecture-tests"

require_test_discovery "$test_root/linux-worker-architecture-tests" "linux_launcher_is_exactly_scoped_and_has_no_path_spawn_fallback"
(
  cd "$repo_root"
  "$test_root/linux-worker-architecture-tests"
)

rustc --edition=2024 --test \
  "$repo_root/src/linux_gpu.rs" \
  -D warnings -A dead-code \
  -o "$test_root/linux-gpu-routing-tests"

require_test_discovery "$test_root/linux-gpu-routing-tests" "tests::canonical_pci_identity_is_fixed_width_and_preserves_nonzero_domain"
"$test_root/linux-gpu-routing-tests" --test-threads=1

rustc --edition=2024 --test \
  "$repo_root/src/linux_gpu_architecture_guard.rs" \
  -D warnings \
  -o "$test_root/linux-gpu-architecture-tests"

require_test_discovery "$test_root/linux-gpu-architecture-tests" "linux_gpu_identity_is_pci_only_fixed_root_and_bounded"
(
  cd "$repo_root"
  "$test_root/linux-gpu-architecture-tests"
)

rustc --edition=2024 --test \
  "$repo_root/src/gpu_worker_pack/linux_release_epoch.rs" \
  -D warnings -A dead-code \
  -o "$test_root/linux-release-epoch-tests"

epoch_portable_tests=(
  "tests::ancestor_owner_and_mode_rules_are_fail_closed"
  "tests::candidate_above_floor_is_uncommitted"
  "tests::candidate_below_floor_is_downgrade"
  "tests::candidate_bounds_are_enforced"
  "tests::canonical_floor_and_equal_candidate_are_accepted"
  "tests::errors_are_path_free"
  "tests::floor_type_link_owner_mode_and_size_rules_are_fail_closed"
  "tests::malformed_digit_is_rejected"
  "tests::maximum_floor_is_accepted"
  "tests::overflow_floor_is_rejected"
  "tests::target_prefix_is_exact"
  "tests::trailing_or_missing_bytes_are_rejected"
  "tests::zero_floor_is_rejected"
)
epoch_native_tests=(
  "linux::tests::ancestor_replacement_is_rejected"
  "linux::tests::concurrent_rechecks_use_independent_positional_reads"
  "linux::tests::deterministic_change_after_first_rebind_is_rejected"
  "linux::tests::deterministic_change_after_retained_read_is_rejected"
  "linux::tests::directory_fifo_and_socket_floor_nodes_are_rejected_without_blocking"
  "linux::tests::fixture_owner_policy_is_enforced_without_chown"
  "linux::tests::fixture_reservation_rejects_directory_and_symlink_collisions_without_mutation"
  "linux::tests::hardlinked_floor_is_rejected"
  "linux::tests::missing_authority_is_rejected"
  "linux::tests::mutation_truncate_and_growth_are_rejected"
  "linux::tests::retained_descriptor_metadata_change_is_rejected"
  "linux::tests::same_content_inode_replacement_is_rejected"
  "linux::tests::symlink_file_and_ancestor_are_rejected"
  "linux::tests::unsafe_directory_and_file_modes_are_rejected"
)
if (( ${#epoch_portable_tests[@]} != 13 || ${#epoch_native_tests[@]} != 14 )); then
  echo "Canonical Linux release epoch test inventory has the wrong count" >&2
  exit 1
fi
require_exact_test_set \
  "$test_root/linux-release-epoch-tests" \
  "${epoch_portable_tests[@]}" \
  "${epoch_native_tests[@]}"
timeout 60s "$test_root/linux-release-epoch-tests" --test-threads=1

rustc --edition=2024 --test \
  "$repo_root/src/linux_release_epoch_architecture_guard.rs" \
  -D warnings \
  -o "$test_root/linux-release-epoch-architecture-tests"

epoch_architecture_tests=(
  "lease_retains_and_rebinds_every_authority_object"
  "linux_release_epoch_authority_is_fixed_private_and_read_only"
  "portable_and_native_epoch_suites_are_discovered_on_both_ubuntu_lanes"
  "reader_grants_no_pack_authentication_or_update_authority"
)
if (( ${#epoch_architecture_tests[@]} != 4 )); then
  echo "Canonical Linux release epoch architecture test inventory has the wrong count" >&2
  exit 1
fi
require_exact_test_set \
  "$test_root/linux-release-epoch-architecture-tests" \
  "${epoch_architecture_tests[@]}"
(
  cd "$repo_root"
  "$test_root/linux-release-epoch-architecture-tests"
)
