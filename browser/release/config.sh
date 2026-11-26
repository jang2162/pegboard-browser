# Settings of the release (release.sh and its tests source this file; it runs nothing).
# shellcheck shell=bash
# The variables are for the scripts that source this file.
# shellcheck disable=SC2034

# The notarytool keychain profile a person creates once (xcrun notarytool store-credentials).
NOTARY_PROFILE="pegboard-notary"
# The Sparkle channel of canary items; stable items have no channel (Sparkle's default).
CANARY_CHANNEL="canary"
# Seconds between the app's automatic update checks (SUScheduledCheckInterval).
CHECK_INTERVAL=86400
# The appcast's file name on the gh-pages branch.
APPCAST_FILE="appcast.xml"
# How many items the appcast keeps (the newest).
APPCAST_KEEP=20
# How long publish waits for GitHub Pages to serve the pushed appcast.
PAGES_WAIT_SECS=600
# The most parallel jobs a build may use: more has hung the build machine (kernel panic).
MAX_BUILD_JOBS=8
# The cargo target directory of the release build of pegboard (relative to the repository):
# its own, so nothing built without PEGBOARD_RELEASE_VERSION is reused.
RELEASE_CARGO_TARGET_DIR="target/release-cargo"
