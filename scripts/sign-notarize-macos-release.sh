#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

app='' archive=''
while (($#)); do case "$1" in --app) app="${2:-}"; shift 2;; --archive-output) archive="${2:-}"; shift 2;; -h|--help) echo 'Usage: sign-notarize-macos-release.sh --app <Scribe.app> [--archive-output <zip>]'; exit 0;; *) echo "unknown argument: $1" >&2; exit 2;; esac; done
[[ "$(uname -s)" == Darwin ]] || { echo 'notarization requires macOS.' >&2; exit 1; }
[[ -d "$app" && ! -L "$app" ]] || { echo 'app is missing or unsafe.' >&2; exit 1; }
: "${SCRIBE_MACOS_SIGNING_IDENTITY:?notarization requires a Developer-ID identity from the protected keychain}"
: "${SCRIBE_MACOS_NOTARY_PROFILE:?notarization requires a protected notarytool keychain profile}"
[[ "$SCRIBE_MACOS_SIGNING_IDENTITY" != '-' ]] || { echo 'notarization cannot use an ad hoc identity.' >&2; exit 1; }
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
requested_archive=''
if [[ -n "$archive" ]]; then
  archive_parent="$(dirname "$archive")"
  mkdir -p "$archive_parent"
  requested_archive="$(cd "$archive_parent" && pwd -P)/$(basename "$archive")"
  if [[ -e "$requested_archive" ]]; then
    [[ -f "$requested_archive" && ! -L "$requested_archive" ]] || { echo 'archive output must be a regular non-symlink file.' >&2; exit 1; }
    rm -f "$requested_archive"
  fi
fi
submission_archive="$(mktemp "${TMPDIR:-/tmp}/scribe-notarization.XXXXXX.zip")"
final_archive=''
cleanup() { rm -f "$submission_archive"; if [[ -n "$final_archive" ]]; then rm -f "$final_archive"; fi; }
trap cleanup EXIT
rm -f "$submission_archive"
codesign --verify --strict --verbose=2 "$app"
ditto -c -k --sequesterRsrc --keepParent "$app" "$submission_archive"
xcrun notarytool submit "$submission_archive" --keychain-profile "$SCRIBE_MACOS_NOTARY_PROFILE" --wait
xcrun stapler staple "$app"
xcrun stapler validate "$app"
bash "$repo_root/scripts/verify-macos-release-package.sh" --app "$app" --require-notarization
if [[ -n "$requested_archive" ]]; then
  final_archive="$(mktemp "$archive_parent/.scribe-notarized.XXXXXX.zip")"
  rm -f "$final_archive"
  ditto -c -k --sequesterRsrc --keepParent "$app" "$final_archive"
  [[ ! -e "$requested_archive" && ! -L "$requested_archive" ]] || { echo 'archive output changed during notarization; refusing to overwrite it.' >&2; exit 1; }
  mv "$final_archive" "$requested_archive"
  final_archive=''
fi
echo 'macOS signing, notarization, and stapling completed.'
