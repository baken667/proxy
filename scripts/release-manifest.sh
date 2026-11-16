#!/usr/bin/env bash
# Writes the update manifest the app checks: DIR/latest.json, and its
# signature DIR/latest.json.sig when UPDATE_SIGNING_KEY is set.
#
#   scripts/release-manifest.sh VERSION DIR
#
# DIR holds the release files, named bkn-proxy-<platform>.<ext>, e.g.
# bkn-proxy-macos-aarch64.dmg. The names carry no version on purpose: that is
# what keeps ".../releases/latest/download/<name>" a permanent link.
#
# The signing key is made once and kept secret (a CI secret, never the repo):
#   openssl genpkey -algorithm ed25519 -out update-key.pem
# The public half is baked into the app as BKN_UPDATE_KEY:
#   openssl pkey -in update-key.pem -pubout -outform DER | tail -c 32 | base64
set -euo pipefail
VERSION=$1
DIR=$2

files='{}'
for file in "$DIR"/bkn-proxy-*.*; do
  name=$(basename "$file")
  platform=${name#bkn-proxy-}
  platform=${platform%.*}
  sha=$(shasum -a 256 "$file" | cut -d' ' -f1)
  files=$(jq --arg p "$platform" --arg n "$name" --arg s "$sha" '.[$p] = {name: $n, sha256: $s}' <<<"$files")
done
jq -n --arg v "$VERSION" --argjson f "$files" '{version: $v, files: $f}' >"$DIR/latest.json"

if [ -n "${UPDATE_SIGNING_KEY:-}" ]; then
  # Raw Ed25519 over the exact bytes of latest.json; the app verifies the same.
  openssl pkeyutl -sign -rawin -inkey <(printf '%s\n' "$UPDATE_SIGNING_KEY") -in "$DIR/latest.json" -out "$DIR/latest.json.sig"
else
  echo "UPDATE_SIGNING_KEY is not set: latest.json is unsigned and apps will ignore it" >&2
fi
cat "$DIR/latest.json"
