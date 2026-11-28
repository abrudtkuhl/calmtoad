#!/usr/bin/env bash
# Verify the macOS packaging on a real (tiny) Mach-O binary. No network, no
# Apple credentials, and no real signing identity: without MACOS_SIGN_IDENTITY
# the bundle is ad-hoc signed.
#
#   scripts/test-package-macos.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/calmtoad-package-test.XXXXXX")"
cleanup() {
  for mount in "$WORK/mount" "$WORK/mount-layout"; do
    if [ -d "$mount" ]; then
      hdiutil detach "$mount" >/dev/null 2>&1 || true
    fi
  done
  rm -rf "$WORK"
}
trap cleanup EXIT

# Never pick up a developer's real signing or notarization credentials.
unset MACOS_SIGN_IDENTITY APPLE_ID APPLE_APP_PASSWORD APPLE_TEAM_ID \
  APPLE_KEYCHAIN_PROFILE CALMTOAD_ALLOW_UNNOTARIZED

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

cat >"$WORK/main.c" <<'C'
#include <stdio.h>
int main(void) { puts("calmtoad fixture"); return 0; }
C
cc -o "$WORK/calmtoad" "$WORK/main.c"

CALMTOAD_DMG_LAYOUT=0 "$ROOT/scripts/package-macos.sh" \
  --binary "$WORK/calmtoad" --version 1.2.3 \
  --name Calm-Toad-Mac-Apple-Silicon --out "$WORK/dist" >/dev/null

APP="$WORK/dist/Calm Toad/Calm Toad.app"
ZIP="$WORK/dist/Calm-Toad-Mac-Apple-Silicon.zip"
DMG="$WORK/dist/Calm-Toad-Mac-Apple-Silicon.dmg"

[ -x "$APP/Contents/MacOS/calmtoad" ] || fail "app bundle has no executable"
plutil -lint "$APP/Contents/Info.plist" >/dev/null || fail "Info.plist is not valid"
[ "$(plutil -extract CFBundleShortVersionString raw "$APP/Contents/Info.plist")" = "1.2.3" ] ||
  fail "Info.plist has the wrong version"
codesign --verify --deep --strict "$APP" || fail "bundle signature does not verify"
# The release used to ship a bare linker-signed binary with Info.plist unbound,
# which is what makes macOS call the app "damaged".
SIGN_INFO="$(codesign -dvvv "$APP" 2>&1)"
printf '%s' "$SIGN_INFO" | grep -q "Info.plist entries=" ||
  fail "Info.plist is not sealed into the signature"
echo "ok 1 - builds a valid, sealed .app bundle"

[ -f "$ZIP" ] || fail "updater zip is missing"
ZIP_LISTING="$(unzip -l "$ZIP")"
printf '%s' "$ZIP_LISTING" | grep -q "Calm Toad/Calm Toad.app/Contents/MacOS/calmtoad" ||
  fail "updater zip does not have the layout the in-app updater expects"
echo "ok 2 - keeps the updater zip layout"

[ -f "$DMG" ] || fail "dmg is missing"
hdiutil verify "$DMG" >/dev/null || fail "dmg does not verify"
mkdir -p "$WORK/mount"
hdiutil attach "$DMG" -mountpoint "$WORK/mount" -nobrowse -readonly >/dev/null
[ -d "$WORK/mount/Calm Toad.app" ] || fail "dmg has no app"
[ -L "$WORK/mount/Applications" ] || fail "dmg has no Applications drop link"
RUN_OUTPUT="$("$WORK/mount/Calm Toad.app/Contents/MacOS/calmtoad")"
printf '%s' "$RUN_OUTPUT" | grep -q "calmtoad fixture" ||
  fail "the app inside the dmg does not run"
hdiutil detach "$WORK/mount" >/dev/null
echo "ok 3 - dmg opens with the app and an Applications drop link"

# The Finder layout is written into the read-write staging directory and must be
# carried into the final UDZO image. A fake osascript keeps this deterministic on
# headless runners while exercising the real packaging path.
mkdir -p "$WORK/layout-bin"
cat >"$WORK/layout-bin/osascript" <<'SH'
#!/bin/sh
# package-macos.sh calls: osascript - <staging directory>
# hdiutil keeps .DS_Store only when it has the Finder store magic, so write a
# minimal valid header instead of arbitrary text.
printf '\000\000\000\001Bud1' > "$2/.DS_Store"
SH
chmod +x "$WORK/layout-bin/osascript"
PATH="$WORK/layout-bin:$PATH" "$ROOT/scripts/package-macos.sh" \
  --binary "$WORK/calmtoad" --version 1.2.3 \
  --name Calm-Toad-Mac-Apple-Silicon --out "$WORK/dist-layout" >/dev/null
LAYOUT_DMG="$WORK/dist-layout/Calm-Toad-Mac-Apple-Silicon.dmg"
mkdir -p "$WORK/mount-layout"
hdiutil attach "$LAYOUT_DMG" -mountpoint "$WORK/mount-layout" -nobrowse -readonly >/dev/null
[ -f "$WORK/mount-layout/.DS_Store" ] ||
  fail "the Finder layout was not carried into the dmg"
hdiutil detach "$WORK/mount-layout" >/dev/null
echo "ok 4 - carries the Finder layout into the dmg"

# A Developer ID build must never silently become an unnotarized one.
if MACOS_SIGN_IDENTITY="Developer ID Application: Fixture (TEAMID)" \
  "$ROOT/scripts/package-macos.sh" --binary "$WORK/calmtoad" --version 1.2.3 \
  --name Calm-Toad-Mac-Apple-Silicon --out "$WORK/dist-unsigned" \
  >"$WORK/unnotarized.log" 2>&1; then
  fail "a signed build without notarization credentials succeeded"
fi
grep -q "no notarization credentials" "$WORK/unnotarized.log" ||
  fail "missing notarization credentials did not fail clearly"
[ ! -e "$WORK/dist-unsigned/Calm-Toad-Mac-Apple-Silicon.dmg" ] ||
  fail "the fail-closed build still produced a dmg"
echo "ok 5 - refuses a signed build without notarization credentials"

# Mock only the Apple tools, then assert the real command order: app notarized
# and stapled before the dmg exists, dmg signed, then dmg notarized and stapled.
mkdir -p "$WORK/sign-bin"
CALL_LOG="$WORK/calls.log"
export CALL_LOG
cat >"$WORK/sign-bin/codesign" <<'SH'
#!/bin/sh
printf 'codesign %s\n' "$*" >>"$CALL_LOG"
for arg in "$@"; do
  case "$arg" in
    *.dmg) [ -f "$arg" ] && printf 'dmg-exists\n' >>"$CALL_LOG" ;;
  esac
done
exit 0
SH
cat >"$WORK/sign-bin/xcrun" <<'SH'
#!/bin/sh
printf 'xcrun %s\n' "$*" >>"$CALL_LOG"
exit 0
SH
chmod +x "$WORK/sign-bin/codesign" "$WORK/sign-bin/xcrun"
PATH="$WORK/sign-bin:$PATH" \
  MACOS_SIGN_IDENTITY="Developer ID Application: Fixture (TEAMID)" \
  APPLE_ID="fixture@example.com" APPLE_APP_PASSWORD="fixture-password" \
  APPLE_TEAM_ID="TEAMID" CALMTOAD_DMG_LAYOUT=0 \
  "$ROOT/scripts/package-macos.sh" --binary "$WORK/calmtoad" --version 1.2.3 \
  --name Calm-Toad-Mac-Apple-Silicon --out "$WORK/dist-signed" >/dev/null

notary_lines="$(grep -n '^xcrun notarytool submit' "$CALL_LOG" | cut -d: -f1)"
[ "$(printf '%s\n' "$notary_lines" | wc -l | tr -d ' ')" = "2" ] ||
  fail "expected the app and the dmg to be notarized"
first_submit="$(grep '^xcrun notarytool submit' "$CALL_LOG" | sed -n '1p')"
second_submit="$(grep '^xcrun notarytool submit' "$CALL_LOG" | sed -n '2p')"
case "$first_submit" in
  *.notary.zip\ *) ;;
  *) fail "the app was not submitted as a zip archive" ;;
esac
case "$second_submit" in
  *.dmg\ *) ;;
  *) fail "the dmg was not submitted directly to notarytool" ;;
esac
first_notary="$(printf '%s\n' "$notary_lines" | sed -n '1p')"
second_notary="$(printf '%s\n' "$notary_lines" | sed -n '2p')"
dmg_sign="$(grep -n '^codesign .*\.dmg' "$CALL_LOG" | head -n 1 | cut -d: -f1)"
first_staple="$(grep -n '^xcrun stapler staple' "$CALL_LOG" | head -n 1 | cut -d: -f1)"
second_staple="$(grep -n '^xcrun stapler staple' "$CALL_LOG" | sed -n '2p' | cut -d: -f1)"
[ -n "$dmg_sign" ] || fail "the dmg was not signed"
[ "$first_notary" -lt "$first_staple" ] || fail "the app was stapled before notarization"
[ "$first_staple" -lt "$dmg_sign" ] || fail "the dmg was signed before the app was stapled"
[ "$dmg_sign" -lt "$second_notary" ] || fail "the dmg was notarized before it was signed"
[ "$second_notary" -lt "$second_staple" ] || fail "the dmg was stapled before notarization"
grep -q '^dmg-exists$' "$CALL_LOG" || fail "the dmg was signed before it existed"
echo "ok 6 - signs and notarizes the app before the dmg, then the dmg itself"

echo "1..6"
echo "All macOS packaging tests passed."
