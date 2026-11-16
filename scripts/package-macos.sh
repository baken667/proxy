#!/usr/bin/env bash
# Builds bkn-proxy.app and a .dmg for one Mac architecture, into dist/.
#
#   BKN_API_URL=https://example.com scripts/package-macos.sh [aarch64|x86_64]
#
# Environment:
#   BKN_API_URL           required  the server the app talks to (baked into the build)
#   BKN_UPDATE_KEY        optional  public key for update checks; without it the app never checks
#   APPLE_SIGN_IDENTITY   optional  e.g. "Developer ID Application: Name (TEAMID)".
#                                   Without it the app is ad-hoc signed: fine on this
#                                   Mac, blocked by Gatekeeper on anyone else's.
#   Notarization, when signing with a Developer ID (one of):
#     APPLE_NOTARY_PROFILE          a profile saved with `xcrun notarytool store-credentials`
#     APPLE_API_KEY_PATH + APPLE_API_KEY_ID + APPLE_API_ISSUER   an App Store Connect API key (CI)
set -euo pipefail
cd "$(dirname "$0")/.."

ARCH=${1:-$(uname -m | sed 's/arm64/aarch64/')}
TARGET=$ARCH-apple-darwin
: "${BKN_API_URL:?set BKN_API_URL to the server address, e.g. https://example.com}"
VERSION=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "bkn-proxy").version')

rustup target add "$TARGET" >/dev/null
cargo build --release --locked -p bkn-proxy --target "$TARGET"

APP=dist/$ARCH/bkn-proxy.app
DMG=dist/bkn-proxy-macos-$ARCH.dmg
rm -rf "dist/$ARCH" "$DMG"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/$TARGET/release/bkn-proxy" "$APP/Contents/MacOS/"
cp assets/icon.icns "$APP/Contents/Resources/"
cat >"$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>kz.bkn.proxy</string>
  <key>CFBundleName</key><string>bkn-proxy</string>
  <key>CFBundleDisplayName</key><string>bkn-proxy</string>
  <key>CFBundleExecutable</key><string>bkn-proxy</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST

if [ -n "${APPLE_SIGN_IDENTITY:-}" ]; then
  # Hardened runtime and a secure timestamp are what notarization requires.
  codesign --force --options runtime --timestamp --sign "$APPLE_SIGN_IDENTITY" "$APP"
else
  echo "APPLE_SIGN_IDENTITY is not set: ad-hoc signing (this Mac only)" >&2
  codesign --force --sign - "$APP"
fi

# The disk image a user sees: the app next to a shortcut to Applications.
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -quiet -volname bkn-proxy -srcfolder "$STAGE" -ov -format UDZO "$DMG"

if [ -n "${APPLE_SIGN_IDENTITY:-}" ]; then
  codesign --force --timestamp --sign "$APPLE_SIGN_IDENTITY" "$DMG"
  if [ -n "${APPLE_NOTARY_PROFILE:-}" ]; then
    xcrun notarytool submit "$DMG" --keychain-profile "$APPLE_NOTARY_PROFILE" --wait
  elif [ -n "${APPLE_API_KEY_PATH:-}" ]; then
    xcrun notarytool submit "$DMG" --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER" --wait
  else
    echo "signed but NOT notarized: set APPLE_NOTARY_PROFILE or the APPLE_API_* variables" >&2
    exit 0
  fi
  xcrun stapler staple "$DMG" # so the check also passes offline
fi
echo "$DMG"
