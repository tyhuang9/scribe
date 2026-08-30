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
codesign --verify --strict --verbose=2 "$app"
xcrun notarytool submit "$app" --keychain-profile "$SCRIBE_MACOS_NOTARY_PROFILE" --wait
xcrun stapler staple "$app"
xcrun stapler validate "$app"
bash "$repo_root/scripts/verify-macos-release-package.sh" --app "$app" --require-notarization
if [[ -n "$archive" ]]; then rm -f "$archive"; ditto -c -k --sequesterRsrc --keepParent "$app" "$archive"; fi
echo 'macOS signing, notarization, and stapling completed.'
