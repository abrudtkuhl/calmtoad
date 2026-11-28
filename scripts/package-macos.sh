#!/bin/sh
# Build the macOS downloads: a signed .app, the updater .zip, and a double-click
# .dmg installer.
#
#   scripts/package-macos.sh --binary target/release/calmtoad --version 0.10.0 \
#     --name Calm-Toad-Mac-Apple-Silicon --out dist
#
# Signing is enabled by MACOS_SIGN_IDENTITY (for example,
# "Developer ID Application: Your Name (TEAMID)"). A signed build must also be
# notarized with either APPLE_ID + APPLE_APP_PASSWORD + APPLE_TEAM_ID or, for
# local use, APPLE_KEYCHAIN_PROFILE. CALMTOAD_ALLOW_UNNOTARIZED=1 explicitly
# permits a local signed-but-unnotarized build. Without an identity the bundle
# is ad-hoc signed, which keeps Gatekeeper's wording at "unidentified developer"
# instead of "damaged", and the DMG is still produced.
set -eu

BINARY=""
VERSION=""
NAME=""
OUT=dist
while [ $# -gt 0 ]; do
  case "$1" in
    --binary) BINARY="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --name) NAME="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done
[ -n "$BINARY" ] && [ -n "$VERSION" ] && [ -n "$NAME" ] || {
  echo "usage: $0 --binary PATH --version X.Y.Z --name NAME [--out DIR]" >&2
  exit 2
}
[ -f "$BINARY" ] || { echo "missing binary: $BINARY" >&2; exit 1; }

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PACKAGE="$OUT/Calm Toad"
APP="$PACKAGE/Calm Toad.app"
ENTITLEMENTS="$ROOT/assets/entitlements.plist"
IDENTITY="${MACOS_SIGN_IDENTITY:-}"
NOTARY_MODE=none

# Never silently turn a Developer ID release into an unnotarized one. CI can
# supply Apple ID credentials directly; a keychain profile is opt-in for local
# builds and is not assumed to exist.
if [ -n "$IDENTITY" ]; then
  if [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_APP_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
    NOTARY_MODE=apple-id
  elif [ -n "${APPLE_KEYCHAIN_PROFILE:-}" ]; then
    NOTARY_MODE=keychain-profile
  elif [ "${CALMTOAD_ALLOW_UNNOTARIZED:-0}" != "1" ]; then
    cat >&2 <<'EOF'
error: MACOS_SIGN_IDENTITY is set but no notarization credentials are available.
Set APPLE_ID, APPLE_APP_PASSWORD, and APPLE_TEAM_ID, or set APPLE_KEYCHAIN_PROFILE.
For a local unnotarized build, explicitly set CALMTOAD_ALLOW_UNNOTARIZED=1.
EOF
    exit 1
  fi
fi

sign_app_with_identity() {
  if [ -f "$ENTITLEMENTS" ]; then
    codesign --force --options runtime --timestamp \
      --entitlements "$ENTITLEMENTS" --sign "$IDENTITY" "$1"
  else
    codesign --force --options runtime --timestamp \
      --sign "$IDENTITY" "$1"
  fi
}

notarize_and_staple() {
  _notary_target="$1"
  _notary_zip=""
  case "$_notary_target" in
    *.dmg)
      # notarytool accepts a disk image directly; wrapping it in a zip can make
      # the submission look like it contains no notarizable bundle.
      _notary_submit="$_notary_target"
      ;;
    *)
      _notary_zip="$OUT/$NAME.notary.zip"
      rm -f "$_notary_zip"
      if ! ditto -c -k --keepParent "$_notary_target" "$_notary_zip"; then
        rm -f "$_notary_zip"
        return 1
      fi
      _notary_submit="$_notary_zip"
      ;;
  esac

  case "$NOTARY_MODE" in
    apple-id)
      echo "==> Notarizing $_notary_target with Apple ID"
      xcrun notarytool submit "$_notary_submit" \
        --apple-id "$APPLE_ID" --password "$APPLE_APP_PASSWORD" --team-id "$APPLE_TEAM_ID" \
        --wait --timeout 30m
      ;;
    keychain-profile)
      echo "==> Notarizing $_notary_target with a local keychain profile"
      xcrun notarytool submit "$_notary_submit" \
        --keychain-profile "$APPLE_KEYCHAIN_PROFILE" --wait --timeout 30m
      ;;
    *)
      echo "internal error: notarization requested without credentials" >&2
      return 1
      ;;
  esac

  if [ -n "$_notary_zip" ]; then
    rm -f "$_notary_zip"
  fi
  xcrun stapler staple "$_notary_target"
  xcrun stapler validate "$_notary_target"
}

rm -rf "$PACKAGE"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$OUT"
cp "$BINARY" "$APP/Contents/MacOS/calmtoad"
chmod 755 "$APP/Contents/MacOS/calmtoad"
if [ -f "$ROOT/README.md" ]; then
  cp "$ROOT/README.md" "$PACKAGE/README.md"
fi

cat >"$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>calmtoad</string>
  <key>CFBundleIdentifier</key><string>app.calmtoad.desktop</string>
  <key>CFBundleName</key><string>Calm Toad</string>
  <key>CFBundleDisplayName</key><string>Calm Toad</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

if [ -n "$IDENTITY" ]; then
  echo "==> Signing with Developer ID (hardened runtime)"
  # Inner executable first, then the bundle, so Info.plist is sealed.
  sign_app_with_identity "$APP/Contents/MacOS/calmtoad"
  sign_app_with_identity "$APP"
else
  echo "==> No Developer ID identity; ad-hoc signing the bundle"
  # Shipping a bare linker-signed binary leaves Info.plist unsealed, which is
  # what makes macOS call the app "damaged". Seal the bundle instead.
  codesign --force -s - "$APP/Contents/MacOS/calmtoad"
  codesign --force -s - "$APP"
fi
codesign --verify --deep --strict --verbose=2 "$APP"

NOTARIZED=0
if [ -n "$IDENTITY" ] && [ "$NOTARY_MODE" != "none" ]; then
  # Notarize and staple the app before putting it in either download format.
  notarize_and_staple "$APP"
  NOTARIZED=1
elif [ -n "$IDENTITY" ]; then
  echo "==> WARNING: CALMTOAD_ALLOW_UNNOTARIZED=1; building without notarization" >&2
fi

# The updater keeps downloading this exact zip layout.
echo "==> Building $NAME.zip"
rm -f "$OUT/$NAME.zip"
ditto -c -k --sequesterRsrc --keepParent "$PACKAGE" "$OUT/$NAME.zip"

# The human download: open, drag to Applications, done.
echo "==> Building $NAME.dmg"
STAGING="$OUT/dmg-staging"
rm -rf "$STAGING"
mkdir -p "$STAGING"
ditto "$APP" "$STAGING/Calm Toad.app"
ln -s /Applications "$STAGING/Applications"

# Ask Finder to write .DS_Store into the read-write source directory. Building
# the UDZO afterwards carries that layout into the final, read-only image.
if [ "${CALMTOAD_DMG_LAYOUT:-1}" = "1" ]; then
  if osascript - "$STAGING" >/dev/null 2>&1 <<'APPLESCRIPT'
on run argv
  tell application "Finder"
    set stagingFolder to (POSIX file (item 1 of argv)) as alias
    open stagingFolder
    delay 1
    tell container window
      set current view to icon view
      set toolbar visible to false
      set statusbar visible to false
      set the bounds to {200, 200, 760, 480}
      set theViewOptions to the icon view options
      set arrangement of theViewOptions to not arranged
      set icon size of theViewOptions to 96
      set position of item "Calm Toad.app" to {150, 150}
      set position of item "Applications" to {410, 150}
      close
    end tell
    delay 1
  end tell
end run
APPLESCRIPT
  then
    if [ ! -f "$STAGING/.DS_Store" ]; then
      echo "warning: Finder did not create a .DS_Store; the DMG will use its default layout" >&2
    fi
  else
    echo "warning: Finder layout failed; building the DMG without a saved window layout" >&2
  fi
fi

DMG="$OUT/$NAME.dmg"
rm -f "$DMG"
hdiutil create -volname "Calm Toad" -srcfolder "$STAGING" -ov -format UDZO \
  -imagekey zlib-level=9 "$DMG" >/dev/null
rm -rf "$STAGING"

if [ -n "$IDENTITY" ]; then
  echo "==> Signing $NAME.dmg with Developer ID and a timestamp"
  codesign --force --sign "$IDENTITY" --timestamp "$DMG"
  codesign --verify --strict --verbose=2 "$DMG"
  if [ "$NOTARY_MODE" != "none" ]; then
    notarize_and_staple "$DMG"
  fi
fi

hdiutil verify "$DMG" >/dev/null
echo "==> Done: $OUT/$NAME.zip and $OUT/$NAME.dmg (notarized: $NOTARIZED)"
