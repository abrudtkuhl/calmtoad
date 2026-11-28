#!/usr/bin/env bash
# Exercise scripts/install.sh against a local, signed fixture release. No network.
#
#   scripts/test-install.sh
#
# Requires minisign and python3. Builds real zip/tar.gz downloads, generates and
# signs manifests, then installs from file:// URLs into temporary directories.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/calmtoad-install-test.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

PASS=0
fail() {
  echo "FAIL: $*" >&2
  exit 1
}
ok() {
  PASS=$((PASS + 1))
  echo "ok $PASS - $*"
}
command -v minisign >/dev/null 2>&1 || fail "minisign is required to create signed fixtures"
command -v python3 >/dev/null 2>&1 || fail "python3 is required to rewrite fixture manifests"

# Treat the machine as idle by default so a developer's running copy does not
# change the outcome. The running-app test overrides this with fake-running.
mkdir -p "$WORK/fake-idle"
cat >"$WORK/fake-idle/pgrep" <<'SH'
#!/bin/sh
exit 1
SH
chmod +x "$WORK/fake-idle/pgrep"
PATH="$WORK/fake-idle:$PATH"
export PATH

minisign -G -W -p "$WORK/test.pub" -s "$WORK/test.key" >/dev/null 2>&1
CALMTOAD_TEST_MODE=1
export CALMTOAD_TEST_MODE CALMTOAD_PUBLIC_KEY
CALMTOAD_PUBLIC_KEY="$(tail -n 1 "$WORK/test.pub")"

point_manifest_at() {
  python3 - "$1/latest.json" "file://$1" <<'PY'
import json, sys
path, base = sys.argv[1], sys.argv[2]
with open(path) as handle:
    data = json.load(handle)
for platform in data["platforms"].values():
    platform["url"] = base + "/" + platform["url"].rsplit("/", 1)[-1]
with open(path, "w") as handle:
    json.dump(data, handle, indent=2)
PY
}

sign_manifest() {
  minisign -q -Sm "$1/latest.json" -s "$WORK/test.key" -t "Calm Toad fixture release"
}

# Point one platform's signed manifest entry at a replacement archive.
rewrite_platform() {
  if command -v shasum >/dev/null 2>&1; then
    digest="$(shasum -a 256 "$3" | awk '{ print $1 }')"
  else
    digest="$(sha256sum "$3" | awk '{ print $1 }')"
  fi
  size="$(wc -c <"$3" | tr -d '[:space:]')"
  python3 - "$1/latest.json" "$2" "$digest" "$size" <<'PY'
import json, sys
manifest, platform, digest, size = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
with open(manifest) as handle:
    data = json.load(handle)
data["platforms"][platform]["sha256"] = digest
data["platforms"][platform]["size"] = size
with open(manifest, "w") as handle:
    json.dump(data, handle, indent=2)
PY
  sign_manifest "$1"
}

make_release() {
  local version="$1"
  local dest="$WORK/releases/download/v$version"
  local package app
  mkdir -p "$dest"
  package="$WORK/build-$version/Calm Toad"
  app="$package/Calm Toad.app"
  mkdir -p "$app/Contents/MacOS"
  cat >"$app/Contents/MacOS/calmtoad" <<EOF
#!/bin/sh
echo "calmtoad $version"
EOF
  chmod +x "$app/Contents/MacOS/calmtoad"
  printf 'name\n' >"$app/Contents/Info.plist"
  printf 'fixture\n' >"$package/README.md"

  if command -v ditto >/dev/null 2>&1; then
    ditto -c -k --sequesterRsrc --keepParent "$package" "$dest/Calm-Toad-Mac-Apple-Silicon.zip"
    ditto -c -k --sequesterRsrc --keepParent "$package" "$dest/Calm-Toad-Mac-Intel.zip"
  else
    (cd "$(dirname "$package")" && zip -qr "$dest/Calm-Toad-Mac-Apple-Silicon.zip" "Calm Toad")
    (cd "$(dirname "$package")" && zip -qr "$dest/Calm-Toad-Mac-Intel.zip" "Calm Toad")
  fi
  (cd "$(dirname "$package")" && zip -qr "$dest/Calm-Toad-Windows.zip" "Calm Toad")

  mkdir -p "$WORK/build-$version-linux/Calm Toad"
  cp "$app/Contents/MacOS/calmtoad" "$WORK/build-$version-linux/Calm Toad/Calm Toad"
  tar -C "$WORK/build-$version-linux" -czf "$dest/Calm-Toad-Linux.tar.gz" "Calm Toad"

  printf -- '- Fixture release %s\n' "$version" >"$WORK/notes-$version.md"
  "$ROOT/scripts/release-manifest.sh" "$version" "v$version" "$WORK/notes-$version.md" "$dest" >/dev/null
  point_manifest_at "$dest"
  sign_manifest "$dest"
}

make_release 1.2.3
make_release 2.0.0
mkdir -p "$WORK/releases/latest"
ln -s "$WORK/releases/download/v2.0.0" "$WORK/releases/latest/download"

export CALMTOAD_RELEASES_URL="file://$WORK/releases"
export CALMTOAD_NO_LAUNCH=1
export HOME="$WORK/home"
mkdir -p "$HOME"
PREFIX="$WORK/Applications"
export CALMTOAD_PREFIX="$PREFIX"
mkdir -p "$PREFIX/unrelated-app"
printf 'keep\n' >"$PREFIX/unrelated-app/sentinel"

case "$(uname -s)" in
  Darwin) INSTALLED="$PREFIX/Calm Toad.app/Contents/MacOS/calmtoad" ;;
  Linux) INSTALLED="$PREFIX/calmtoad/Calm Toad/Calm Toad" ;;
esac

sh "$ROOT/scripts/install.sh" >/dev/null
[ -x "$INSTALLED" ] || fail "latest install produced no executable"
[ "$("$INSTALLED")" = "calmtoad 2.0.0" ] || fail "latest install ran the wrong version"
[ -f "$PREFIX/unrelated-app/sentinel" ] || fail "install deleted unrelated prefix content"
ok "installs a signed latest release without touching unrelated content"

sh "$ROOT/scripts/install.sh" --version 1.2.3 >/dev/null
[ "$("$INSTALLED")" = "calmtoad 1.2.3" ] || fail "pinned install ran the wrong version"
ok "installs a pinned version"

sh "$ROOT/scripts/install.sh" >/dev/null
[ "$("$INSTALLED")" = "calmtoad 2.0.0" ] || fail "reinstall did not upgrade"
ok "reinstalls over an existing copy"

if [ "$(uname -s)" = "Darwin" ]; then
  xattr -w com.apple.quarantine "0083;00000000;Test;" "$PREFIX/Calm Toad.app"
  xattr -w com.apple.quarantine "0083;00000000;Test;" "$INSTALLED"
  sh "$ROOT/scripts/install.sh" >/dev/null
  if xattr -p com.apple.quarantine "$PREFIX/Calm Toad.app" >/dev/null 2>&1; then
    fail "quarantine flag survived the install"
  fi
  if xattr -p com.apple.quarantine "$INSTALLED" >/dev/null 2>&1; then
    fail "nested quarantine flag survived the install"
  fi
  ok "clears quarantine attributes that were actually present"
fi

# The archive is tampered after its manifest was signed.
TAMPERED="$WORK/releases/download/v9.9.9"
cp -R "$WORK/releases/download/v2.0.0" "$TAMPERED"
point_manifest_at "$TAMPERED"
sign_manifest "$TAMPERED"
printf 'x' >>"$TAMPERED/Calm-Toad-Mac-Apple-Silicon.zip"
printf 'x' >>"$TAMPERED/Calm-Toad-Mac-Intel.zip"
printf 'x' >>"$TAMPERED/Calm-Toad-Linux.tar.gz"
if sh "$ROOT/scripts/install.sh" --version 9.9.9 >/dev/null 2>&1; then
  fail "a tampered download was installed"
fi
[ "$("$INSTALLED")" = "calmtoad 2.0.0" ] || fail "tampered install changed the existing app"
ok "refuses a download whose checksum does not match"

# A modified manifest must fail even when its embedded checksum still matches.
BAD_SIGNATURE="$WORK/releases/download/v3.0.0"
cp -R "$WORK/releases/download/v2.0.0" "$BAD_SIGNATURE"
point_manifest_at "$BAD_SIGNATURE"
printf '\n' >>"$BAD_SIGNATURE/latest.json"
if sh "$ROOT/scripts/install.sh" --version 3.0.0 >"$WORK/bad-signature.log" 2>&1; then
  fail "a manifest with a bad signature was installed"
fi
grep -q "signature verification" "$WORK/bad-signature.log" || fail "manifest did not fail at signature verification"
[ "$("$INSTALLED")" = "calmtoad 2.0.0" ] || fail "bad signature changed the existing app"
ok "refuses a manifest whose minisign signature is invalid"

# Production trust anchors cannot be replaced through ordinary environment
# variables. The fixture is signed by a generated test key, not Calm Toad's key.
if CALMTOAD_TEST_MODE=0 \
  CALMTOAD_PUBLIC_KEY="$CALMTOAD_PUBLIC_KEY" \
  CALMTOAD_MINISIGN_URL="http://attacker.invalid/minisign" \
  CALMTOAD_MINISIGN_SHA256="0000000000000000000000000000000000000000000000000000000000000000" \
  sh "$ROOT/scripts/install.sh" >/dev/null 2>&1; then
  fail "a caller replaced the production signing/verifier trust anchors"
fi
[ "$("$INSTALLED")" = "calmtoad 2.0.0" ] || fail "trust-anchor override changed the existing app"
ok "production trust anchors cannot be overridden by environment variables"

python3 - "$WORK/releases/download/v2.0.0/latest.json" <<'PY'
import json, sys
path = sys.argv[1]
with open(path) as handle:
    data = json.load(handle)
with open(path, "w") as handle:
    json.dump(data, handle, separators=(",", ":"))
PY
sign_manifest "$WORK/releases/download/v2.0.0"
sh "$ROOT/scripts/install.sh" >/dev/null
[ "$("$INSTALLED")" = "calmtoad 2.0.0" ] || fail "compact manifest install failed"
ok "parses a compact, signed manifest"

# Use a separate copy: the missing-checksum test must not corrupt the release
# that later tests (running app, rollback, Linux prefix) install from.
NO_CHECKSUM="$WORK/releases/download/v4.0.0"
cp -R "$WORK/releases/download/v2.0.0" "$NO_CHECKSUM"
point_manifest_at "$NO_CHECKSUM"
python3 - "$NO_CHECKSUM/latest.json" <<'PY'
import json, sys
path = sys.argv[1]
with open(path) as handle:
    data = json.load(handle)
for platform in data["platforms"].values():
    platform.pop("sha256", None)
with open(path, "w") as handle:
    json.dump(data, handle)
PY
sign_manifest "$NO_CHECKSUM"
if sh "$ROOT/scripts/install.sh" --version 4.0.0 >"$WORK/no-checksum.log" 2>&1; then
  fail "installed without a checksum in the signed manifest"
fi
grep -q "checksum" "$WORK/no-checksum.log" || fail "missing checksum did not fail at checksum validation"
ok "refuses a signed manifest with no checksum"

if [ "$(uname -s)" = "Darwin" ]; then
  mkdir -p "$WORK/fake-running"
  cat >"$WORK/fake-running/pgrep" <<'SH'
#!/bin/sh
exit 0
SH
  chmod +x "$WORK/fake-running/pgrep"
  if PATH="$WORK/fake-running:$PATH" sh "$ROOT/scripts/install.sh" >/dev/null 2>&1; then
    fail "installer replaced a running app"
  fi
  [ "$("$INSTALLED")" = "calmtoad 2.0.0" ] || fail "running-app refusal changed the app"
  ok "refuses to replace a running app"

  mkdir -p "$WORK/fake-mv"
  cat >"$WORK/fake-mv/mv" <<'SH'
#!/bin/sh
case "$1" in
  *.new.*) exit 1 ;;
esac
exec /bin/mv "$@"
SH
  chmod +x "$WORK/fake-mv/mv"
  if PATH="$WORK/fake-mv:$PATH" sh "$ROOT/scripts/install.sh" >/dev/null 2>&1; then
    fail "injected swap failure unexpectedly succeeded"
  fi
  [ "$("$INSTALLED")" = "calmtoad 2.0.0" ] || fail "failed swap did not restore the old app"
  ok "restores the previous app after a failed swap"

  # Interrupt after the old app has moved aside but before the new app is moved
  # into place. The EXIT restoration path must still put the original back.
  mkdir -p "$WORK/fake-mv-signal"
  cat >"$WORK/fake-mv-signal/mv" <<'SH'
#!/bin/sh
case "$1" in
  *.new.*) kill -TERM "$PPID"; exit 1 ;;
esac
exec /bin/mv "$@"
SH
  chmod +x "$WORK/fake-mv-signal/mv"
  if PATH="$WORK/fake-mv-signal:$PATH" sh "$ROOT/scripts/install.sh" >/dev/null 2>&1; then
    fail "interrupted swap unexpectedly succeeded"
  fi
  [ "$("$INSTALLED")" = "calmtoad 2.0.0" ] || fail "interrupted swap did not restore the old app"
  ok "restores the previous app after an interrupted swap"

  # SIGKILL cannot run EXIT; the next invocation must recover the known backup.
  mkdir -p "$WORK/fake-mv-kill"
  KILL_ONCE="$WORK/killed-once"
  cat >"$WORK/fake-mv-kill/mv" <<'SH'
#!/bin/sh
case "$1" in
  *.new.*)
    if [ ! -e "$KILL_ONCE" ]; then
      : >"$KILL_ONCE"
      kill -KILL "$PPID"
      exit 1
    fi
    ;;
esac
exec /bin/mv "$@"
SH
  chmod +x "$WORK/fake-mv-kill/mv"
  if KILL_ONCE="$KILL_ONCE" PATH="$WORK/fake-mv-kill:$PATH" sh "$ROOT/scripts/install.sh" >/dev/null 2>&1; then
    fail "killed swap unexpectedly succeeded"
  fi
  if ! KILL_ONCE="$KILL_ONCE" PATH="$WORK/fake-mv-kill:$PATH" sh "$ROOT/scripts/install.sh" >/dev/null 2>&1; then
    fail "next invocation did not recover after SIGKILL"
  fi
  [ "$("$INSTALLED")" = "calmtoad 2.0.0" ] || fail "SIGKILL recovery did not restore the old app"
  ok "recovers interrupted state on next invocation after SIGKILL"

  # A killed backup-cleanup trap can leave partial old-app debris beside a
  # complete, valid newly installed app. Preserve the app; remove only debris.
  PARTIAL_BACKUP="$PREFIX/.Calm Toad.app.backup.partial"
  mkdir -p "$PARTIAL_BACKUP/Contents/MacOS"
  printf '#!/bin/sh\nexit 1\n' >"$PARTIAL_BACKUP/Contents/MacOS/calmtoad"
  chmod +x "$PARTIAL_BACKUP/Contents/MacOS/calmtoad"
  sh "$ROOT/scripts/install.sh" >/dev/null 2>&1 || fail "partial backup cleanup did not recover"
  [ ! -e "$PARTIAL_BACKUP" ] || fail "partial backup debris was not removed"
  [ "$("$INSTALLED")" = "calmtoad 2.0.0" ] || fail "valid target was not preserved"
  ok "preserves valid target and cleans partial backup"

  # If no valid target remains, restore the complete known backup.
  MISSING_TARGET_BACKUP="$PREFIX/.Calm Toad.app.backup.killed"
  mv "$PREFIX/Calm Toad.app" "$MISSING_TARGET_BACKUP"
  sh "$ROOT/scripts/install.sh" >/dev/null 2>&1 || fail "missing target was not restored"
  [ "$("$INSTALLED")" = "calmtoad 2.0.0" ] || fail "missing target restore failed"
  [ ! -e "$MISSING_TARGET_BACKUP" ] || fail "restored backup was not cleaned"
  ok "restores complete backup when target is missing"
fi

# Exercise the Linux path on every host with a fake uname. In particular, a
# prefix of ~/.local must never move or delete the rest of ~/.local.
mkdir -p "$WORK/fake-linux"
cat >"$WORK/fake-linux/uname" <<'SH'
#!/bin/sh
case "${1:-}" in
  -s) printf 'Linux\n' ;;
  -m) printf 'x86_64\n' ;;
  *) exec /usr/bin/uname "$@" ;;
esac
SH
chmod +x "$WORK/fake-linux/uname"
LINUX_HOME="$WORK/linux-home"
mkdir -p "$LINUX_HOME/.local/other-app"
printf 'do not delete\n' >"$LINUX_HOME/.local/other-app/sentinel"
LINUX_INSTALLED="$LINUX_HOME/.local/calmtoad/Calm Toad/Calm Toad"
PATH="$WORK/fake-linux:$PATH" HOME="$LINUX_HOME" CALMTOAD_PREFIX="$LINUX_HOME/.local" \
  sh "$ROOT/scripts/install.sh" >/dev/null
[ "$("$LINUX_INSTALLED")" = "calmtoad 2.0.0" ] || fail "Linux custom-prefix install failed"
[ -f "$LINUX_HOME/.local/other-app/sentinel" ] || fail "Linux install deleted unrelated ~/.local content"
EXPECTED_EXEC="Exec=\"$LINUX_HOME/.local/bin/calmtoad\""
grep -Fqx "$EXPECTED_EXEC" "$LINUX_HOME/.local/share/applications/calmtoad.desktop" ||
  fail "Linux desktop Exec is not correctly quoted"
if command -v desktop-file-validate >/dev/null 2>&1; then
  desktop-file-validate "$LINUX_HOME/.local/share/applications/calmtoad.desktop"
fi
PATH="$WORK/fake-linux:$PATH" HOME="$LINUX_HOME" CALMTOAD_PREFIX="$LINUX_HOME/.local" \
  sh "$ROOT/scripts/install.sh" >/dev/null
[ -f "$LINUX_HOME/.local/other-app/sentinel" ] || fail "Linux reinstall deleted unrelated ~/.local content"
ok "Linux custom prefixes preserve unrelated data and generate a quoted launcher"

# The same running-app guard protects Linux, for both installed process names.
mkdir -p "$WORK/fake-running-linux"
cat >"$WORK/fake-running-linux/pgrep" <<'SH'
#!/bin/sh
exit 0
SH
chmod +x "$WORK/fake-running-linux/pgrep"
if PATH="$WORK/fake-linux:$WORK/fake-running-linux:$PATH" \
  HOME="$LINUX_HOME" CALMTOAD_PREFIX="$LINUX_HOME/.local" \
  sh "$ROOT/scripts/install.sh" >/dev/null 2>&1; then
  fail "Linux installer replaced a running app"
fi
[ "$("$LINUX_INSTALLED")" = "calmtoad 2.0.0" ] || fail "Linux running-app refusal changed the app"
ok "refuses to replace a running app on Linux"

# A signal in the same narrow window restores Linux's previous installation too.
mkdir -p "$WORK/fake-mv-signal-linux"
cat >"$WORK/fake-mv-signal-linux/mv" <<'SH'
#!/bin/sh
case "$1" in
  *.new.*) kill -TERM "$PPID"; exit 1 ;;
esac
exec /bin/mv "$@"
SH
chmod +x "$WORK/fake-mv-signal-linux/mv"
if PATH="$WORK/fake-linux:$WORK/fake-mv-signal-linux:$PATH" \
  HOME="$LINUX_HOME" CALMTOAD_PREFIX="$LINUX_HOME/.local" \
  sh "$ROOT/scripts/install.sh" >/dev/null 2>&1; then
  fail "interrupted Linux swap unexpectedly succeeded"
fi
[ "$("$LINUX_INSTALLED")" = "calmtoad 2.0.0" ] || fail "interrupted Linux swap did not restore the old app"
ok "restores the previous Linux app after an interrupted swap"

mkdir -p "$LINUX_HOME/custom/calmtoad"
printf 'not an app\n' >"$LINUX_HOME/custom/calmtoad/sentinel"
if PATH="$WORK/fake-linux:$PATH" HOME="$LINUX_HOME" CALMTOAD_PREFIX="$LINUX_HOME/custom" \
  sh "$ROOT/scripts/install.sh" >/dev/null 2>&1; then
  fail "installer replaced an unrecognized destination"
fi
[ -f "$LINUX_HOME/custom/calmtoad/sentinel" ] || fail "unrecognized destination was deleted"
ok "refuses to replace an unrecognized installation directory"

# A signed manifest is not enough: the archive itself must not contain paths that
# escape the staging directory. Re-sign the replacement so only the path check can
# reject it.
UNSAFE="$WORK/releases/download/v5.0.0"
cp -R "$WORK/releases/download/v2.0.0" "$UNSAFE"
point_manifest_at "$UNSAFE"
python3 - "$UNSAFE/Calm-Toad-Linux.tar.gz" <<'PY'
import io, sys, tarfile
path = sys.argv[1]
with tarfile.open(path, "w:gz") as archive:
    entry = tarfile.TarInfo("../escaped")
    entry.size = 1
    archive.addfile(entry, io.BytesIO(b"x"))
PY
rewrite_platform "$UNSAFE" linux-x86_64 "$UNSAFE/Calm-Toad-Linux.tar.gz"
if PATH="$WORK/fake-linux:$PATH" HOME="$LINUX_HOME" CALMTOAD_PREFIX="$LINUX_HOME/.local" \
  sh "$ROOT/scripts/install.sh" --version 5.0.0 >"$WORK/unsafe.log" 2>&1; then
  fail "installed an archive with a traversal path"
fi
grep -q "unsafe path" "$WORK/unsafe.log" || fail "traversal archive did not fail at the path check"
[ "$("$LINUX_INSTALLED")" = "calmtoad 2.0.0" ] || fail "unsafe archive changed the installed app"
[ ! -e "$LINUX_HOME/.local/escaped" ] || fail "traversal archive wrote outside the staging directory"
ok "refuses an archive whose paths escape the install directory"

HELP_OUTPUT="$(sh "$ROOT/scripts/install.sh" --help)"
case "$HELP_OUTPUT" in
  *"set -eu"*) fail "--help leaks the script body" ;;
esac
ok "--help prints only usage text"

# The verifier key baked into the installer must match the committed updater key.
SCRIPT_KEY="$(sed -n 's/^PUBLIC_KEY="\(.*\)"$/\1/p' "$ROOT/scripts/install.sh" | head -n 1)"
FILE_KEY="$(tail -n 1 "$ROOT/assets/updater.pub" | tr -d '\r')"
[ -n "$SCRIPT_KEY" ] || fail "could not find the installer's built-in verifier key"
[ "$SCRIPT_KEY" = "$FILE_KEY" ] || fail "install.sh verifier key does not match assets/updater.pub"
ok "install.sh verifier key matches assets/updater.pub"

echo "1..$PASS"
echo "All installer tests passed."
