#!/bin/sh
# Install Calm Toad on macOS or Linux with one command:
#
#   curl -fsSL https://raw.githubusercontent.com/abrudtkuhl/calmtoad/main/scripts/install.sh | sh
#
# The script picks the right download for this computer, verifies the release's
# minisign signature and SHA-256, installs it, and opens it. On macOS it clears
# the quarantine flag, so there is no right-click-Open workaround. No
# administrator password is needed: when /Applications is not writable it uses
# ~/Applications instead. A running copy is never quit or replaced.
#
# Options (pass after `sh -s --`, or set the matching environment variable):
#   --prefix DIR    install into DIR          (CALMTOAD_PREFIX)
#                   macOS: DIR/Calm Toad.app, Linux: DIR/calmtoad
#   --version X.Y.Z install a specific release (CALMTOAD_VERSION, default latest)
#   --no-launch     install without opening    (CALMTOAD_NO_LAUNCH=1)
#   --help          show this help
#
# CALMTOAD_RELEASES_URL overrides the release location for testing. Production
# trust anchors are hard-coded: the signing key and minisign verifier cannot be
# replaced by environment variables. CALMTOAD_TEST_MODE=1 enables test-only
# injection for offline test suites; never set it for a real installation.
set -eu

REPOSITORY="${CALMTOAD_REPOSITORY:-abrudtkuhl/calmtoad}"
RELEASES="${CALMTOAD_RELEASES_URL:-https://github.com/$REPOSITORY/releases}"
VERSION="${CALMTOAD_VERSION:-latest}"
PREFIX="${CALMTOAD_PREFIX:-}"
PUBLIC_KEY="RWTvc4LCsmQ2iJ3GkIgp9ojZMoVBM9+3wSHqGmMz8cGYbYXF4F1Wow+d"
CALMTOAD_TEST_MODE="${CALMTOAD_TEST_MODE:-0}"
if [ "$CALMTOAD_TEST_MODE" = "1" ] && [ -n "${CALMTOAD_PUBLIC_KEY:-}" ]; then
  PUBLIC_KEY="$CALMTOAD_PUBLIC_KEY"
fi
LAUNCH=1
if [ "${CALMTOAD_NO_LAUNCH:-0}" = "1" ]; then
  LAUNCH=0
fi

MAX_ARCHIVE_BYTES=536870912
MAX_EXTRACTED_BYTES=1073741824
MAX_ARCHIVE_ENTRIES=20000

say() {
  printf '\033[1m==>\033[0m %s\n' "$*"
}

note() {
  printf '    %s\n' "$*"
}

die() {
  printf '\033[31merror:\033[0m %s\n' "$*" >&2
  exit 1
}

usage() {
  if [ -f "$0" ]; then
    sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'
  else
    printf '%s\n' \
      'Install Calm Toad on macOS or Linux.' \
      '  --prefix DIR    install under DIR' \
      '  --version X.Y.Z install a specific release (default latest)' \
      '  --no-launch     install without opening Calm Toad' \
      'Example: curl -fsSL https://raw.githubusercontent.com/abrudtkuhl/calmtoad/main/scripts/install.sh | sh'
  fi
}

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix)
      [ $# -ge 2 ] || die "--prefix needs a directory."
      PREFIX="$2"
      shift 2
      ;;
    --version)
      [ $# -ge 2 ] || die "--version needs a version number."
      VERSION="$2"
      shift 2
      ;;
    --no-launch)
      LAUNCH=0
      shift
      ;;
    --help | -h)
      usage
      exit 0
      ;;
    *)
      die "Unknown option: $1 (try --help)"
      ;;
  esac
done

[ -n "${HOME:-}" ] || die "HOME is not set; run this from your normal user account."

if command -v curl >/dev/null 2>&1; then
  fetch() {
    curl -fsSL --retry 3 --connect-timeout 20 -o "$2" "$1"
  }
elif command -v wget >/dev/null 2>&1; then
  fetch() {
    wget -q -O "$2" "$1"
  }
else
  die "This installer needs curl or wget. Install one of them and try again."
fi

sha256_of() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    die "No SHA-256 tool found (shasum or sha256sum)."
  fi
}

os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Darwin)
    case "$arch" in
      arm64 | aarch64) PLATFORM=macos-aarch64 ;;
      x86_64) PLATFORM=macos-x86_64 ;;
      *) die "This Mac's processor ($arch) is not supported. See $RELEASES/latest for downloads." ;;
    esac
    NAME=Calm-Toad-Mac-Intel.zip
    if [ "$PLATFORM" = "macos-aarch64" ]; then
      NAME=Calm-Toad-Mac-Apple-Silicon.zip
    fi
    MINISIGN_URL="https://github.com/jedisct1/minisign/releases/download/0.11/minisign-0.11-macos.zip"
    MINISIGN_SHA256="e7c410ae8b8960d7087392472b040bda9b2f307c76df0384ac37f9ad103fc893"
    ;;
  Linux)
    case "$arch" in
      x86_64 | amd64) PLATFORM=linux-x86_64 ;;
      *) die "This installer supports macOS and 64-bit Intel/AMD Linux. See $RELEASES/latest for other downloads." ;;
    esac
    NAME=Calm-Toad-Linux.tar.gz
    MINISIGN_URL="https://github.com/jedisct1/minisign/releases/download/0.12/minisign-0.12-linux.tar.gz"
    MINISIGN_SHA256="9a599b48ba6eb7b1e80f12f36b94ceca7c00b7a5173c95c3efc88d9822957e73"
    ;;
  *)
    die "This installer supports macOS and Linux. On Windows, download $RELEASES/latest/download/Calm-Toad-Windows.zip, right-click it, choose Extract All, and open Calm Toad."
    ;;
esac

if [ "$VERSION" = "latest" ]; then
  BASE="$RELEASES/latest/download"
else
  BASE="$RELEASES/download/v$VERSION"
fi

WORK="$(mktemp -d 2>/dev/null || mktemp -d -t calmtoad-install)"

# Transaction state for restoring a replaced app on every exit, including
# SIGINT/SIGTERM. `transaction_original_moved` is set only after the old app has
# safely been moved aside; never remove a still-original target on a failed move.
transaction_target=""
transaction_stage=""
transaction_backup=""
transaction_original_existed=0
transaction_original_moved=0
transaction_started=0
transaction_committed=0

# Recover a previous SIGKILL/power-loss interruption. `mv` is atomic, but a
# killed shell cannot run its EXIT trap; backups and stages can therefore remain.
# Restore the original from the known backup name and clear only our own stages.
recover_interrupted_install() {
  _recovery_prefix="$1"
  _recovery_target="$2"
  _recovery_stage_pattern="$3"
  _recovery_backup_pattern="$4"
  _recovery_relative_executable="$5"
  _recovery_backup=""
  _recovery_count=0

  for _recovery_candidate in "$_recovery_prefix"/* "$_recovery_prefix"/.[!.]* "$_recovery_prefix"/..?*; do
    case "$_recovery_candidate" in
      "$_recovery_prefix"/$_recovery_backup_pattern)
        if [ -d "$_recovery_candidate" ]; then
          _recovery_count=$((_recovery_count + 1))
          _recovery_backup="$_recovery_candidate"
        fi
        ;;
    esac
  done

  if [ "$_recovery_count" -gt 1 ]; then
    die "Multiple interrupted-install backups remain in $_recovery_prefix; inspect them before retrying. Nothing was changed."
  fi

  if [ "$_recovery_count" -eq 1 ]; then
    _recovery_executable="$_recovery_target/$_recovery_relative_executable"
    if [ -f "$_recovery_executable" ] && [ -x "$_recovery_executable" ]; then
      # The atomic rename completed; only old-backup cleanup was interrupted.
      # Keep the valid installed app and discard stale/partial backup debris.
      rm -rf "$_recovery_backup"
      sync
      say "Cleaned an interrupted install."
    else
      _recovery_backup_executable="$_recovery_backup/$_recovery_relative_executable"
      if [ ! -f "$_recovery_backup_executable" ] || [ ! -x "$_recovery_backup_executable" ]; then
        die "Installed app is invalid and backup is incomplete; preserving both. Inspect $_recovery_prefix before retrying. Nothing was changed."
      fi
      # Restore only when the target is absent or invalid.
      if [ -e "$_recovery_target" ] || [ -L "$_recovery_target" ]; then
        rm -rf "$_recovery_target"
      fi
      if ! mv "$_recovery_backup" "$_recovery_target"; then
        die "Could not recover interrupted install. Original is preserved at $_recovery_backup"
      fi
      sync
      say "Recovered an interrupted install."
    fi
  fi

  for _recovery_candidate in "$_recovery_prefix"/* "$_recovery_prefix"/.[!.]* "$_recovery_prefix"/..?*; do
    case "$_recovery_candidate" in
      "$_recovery_prefix"/$_recovery_stage_pattern)
        if [ -e "$_recovery_candidate" ] || [ -L "$_recovery_candidate" ]; then
          rm -rf "$_recovery_candidate"
        fi
        ;;
    esac
  done
}

restore_on_exit() {
  _status=$1
  set +e
  trap '' INT TERM HUP

  if [ -n "${WORK:-}" ]; then
    rm -rf "$WORK"
  fi
  if [ -n "$transaction_stage" ]; then
    rm -rf "$transaction_stage"
  fi

  if [ "$transaction_started" = "1" ] && [ "$transaction_committed" != "1" ]; then
    if [ "$transaction_original_moved" = "1" ]; then
      # A failed rename can leave a partial destination. Remove only that
      # partial/new target before restoring the original, and keep the backup
      # if restoring itself fails.
      if [ -n "$transaction_target" ]; then
        rm -rf "$transaction_target"
      fi
      if [ -e "$transaction_backup" ] || [ -L "$transaction_backup" ]; then
        if ! mv "$transaction_backup" "$transaction_target"; then
          note "Installation failed. Previous installation is preserved at $transaction_backup"
        else
          sync
        fi
      else
        note "Installation failed. Could not restore the previous installation; backup is missing."
      fi
    elif [ "$transaction_original_existed" != "1" ] && [ -n "$transaction_target" ]; then
      # No original was moved aside; remove only a partial new target.
      rm -rf "$transaction_target"
    fi
  fi

  if [ "$transaction_committed" = "1" ] && [ -n "$transaction_backup" ]; then
    rm -rf "$transaction_backup"
    sync
  fi

  exit "$_status"
}

# Clean up temporary downloads and restore any in-flight replacement on every
# exit. Signals become nonzero exits and then run the same restoration path.
trap '_status=$?; restore_on_exit "$_status"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

# Use an installed verifier when possible. Otherwise fetch the official
# universal macOS or static Linux binary into the temporary directory. The
# archive hashes are pinned here, so a compromised Calm Toad release asset
# cannot substitute its own verifier or manifest key.
MINISIGN="$(command -v minisign 2>/dev/null || true)"
if [ -z "$MINISIGN" ]; then
  if [ "$CALMTOAD_TEST_MODE" = "1" ]; then
    MINISIGN_URL="${CALMTOAD_MINISIGN_URL:-$MINISIGN_URL}"
    MINISIGN_SHA256="${CALMTOAD_MINISIGN_SHA256:-$MINISIGN_SHA256}"
  fi
  say "Preparing release-signature verification"
  fetch "$MINISIGN_URL" "$WORK/minisign.archive" ||
    die "Could not download the signature verifier. Install minisign and run this command again."
  ACTUAL="$(sha256_of "$WORK/minisign.archive")"
  [ "$ACTUAL" = "$MINISIGN_SHA256" ] ||
    die "The signature verifier did not match its pinned checksum. Nothing was installed."
  mkdir "$WORK/verifier"
  case "$PLATFORM" in
    macos-*)
      if command -v ditto >/dev/null 2>&1; then
        ditto -x -k "$WORK/minisign.archive" "$WORK/verifier"
      else
        unzip -q "$WORK/minisign.archive" -d "$WORK/verifier"
      fi
      MINISIGN="$WORK/verifier/minisign"
      ;;
    linux-x86_64)
      tar -xzf "$WORK/minisign.archive" -C "$WORK/verifier"
      MINISIGN="$WORK/verifier/minisign-linux/x86_64/minisign"
      ;;
  esac
  [ -f "$MINISIGN" ] || die "The signature verifier archive had an unexpected layout."
  chmod +x "$MINISIGN"
  "$MINISIGN" -v >/dev/null 2>&1 || die "The signature verifier could not start on this computer."
fi

say "Finding Calm Toad for $os ($arch)"
fetch "$BASE/latest.json" "$WORK/latest.json" ||
  die "Could not download the release manifest. Check your internet connection, or get the app from $RELEASES/latest"
fetch "$BASE/latest.json.minisig" "$WORK/latest.json.minisig" ||
  die "The release signature is missing. Nothing was installed."
if ! "$MINISIGN" -q -Vm "$WORK/latest.json" -x "$WORK/latest.json.minisig" -P "$PUBLIC_KEY"; then
  die "The release manifest failed signature verification. Nothing was installed."
fi
note "Release signature verified."

# jq/python/node are not guaranteed to exist. Split the small manifest into one
# token per line, so both pretty-printed and compact JSON parse with awk.
TOKENS="$WORK/manifest.tokens"
sed -e 's/{/\
/g' -e 's/}/\
/g' -e 's/,/\
/g' "$WORK/latest.json" | sed -e 's/^[ \t\r]*//' -e 's/[ \t\r]*$//' >"$TOKENS"

manifest_value() {
  awk -v platform="\"$1\"" -v key="\"$2\"" '
    function scalar(line,   value) {
      value = substr(line, length(key) + 2)
      sub(/^[ \t\r]+/, "", value)
      sub(/[ \t\r]+$/, "", value)
      sub(/^"/, "", value)
      sub(/"$/, "", value)
      return value
    }
    platform == "\"\"" && index($0, key ":") == 1 {
      print scalar($0)
      exit
    }
    $0 == platform ":" { inside = 1; next }
    inside && $0 == "}" { inside = 0 }
    inside && index($0, key ":") == 1 {
      print scalar($0)
      exit
    }
  ' "$TOKENS"
}

RELEASE_VERSION="$(manifest_value "" version)"
URL="$(manifest_value "$PLATFORM" url)"
EXPECTED="$(manifest_value "$PLATFORM" sha256)"
SIZE="$(manifest_value "$PLATFORM" size)"

[ -n "$RELEASE_VERSION" ] || die "The release manifest has no version."
[ -n "$URL" ] || die "There is no $PLATFORM download in this release; see $RELEASES/latest"
case "$URL" in
  */"$NAME") ;;
  *) die "The release manifest points at an unexpected $PLATFORM download." ;;
esac
[ "${#EXPECTED}" -eq 64 ] || die "The release manifest has an invalid checksum."
case "$EXPECTED" in
  *[!0-9a-fA-F]*) die "The release manifest has an invalid checksum." ;;
esac
case "$SIZE" in
  '' | *[!0-9]*) die "The release manifest has an invalid download size." ;;
esac
[ "$SIZE" -gt 0 ] && [ "$SIZE" -le "$MAX_ARCHIVE_BYTES" ] ||
  die "The release manifest has an unsafe download size."

say "Downloading Calm Toad $RELEASE_VERSION"
fetch "$URL" "$WORK/$NAME" || die "The download failed. Try again, or get it from $RELEASES/latest"
ACTUAL_SIZE="$(wc -c <"$WORK/$NAME" | tr -d '[:space:]')"
[ "$ACTUAL_SIZE" = "$SIZE" ] || die "The download size does not match the signed manifest."
ACTUAL="$(sha256_of "$WORK/$NAME")"
[ "$ACTUAL" = "$EXPECTED" ] || die "The download does not match its signed checksum. Nothing was installed."
note "Checksum and size verified."

# Reject traversal, links, excessive entries and archive bombs before handing
# the package to the platform extractor. Official packages contain only regular
# files and directories.
assert_archive_safe() {
  paths="$WORK/archive.paths"
  verbose="$WORK/archive.verbose"
  sizes="$WORK/archive.sizes"
  if [ "$PLATFORM" = "linux-x86_64" ]; then
    tar -tzf "$WORK/$NAME" >"$paths" || die "Could not read the downloaded archive."
    tar -tvzf "$WORK/$NAME" >"$verbose" || die "Could not inspect the downloaded archive."
    # BSD tar prints "perms links owner group size date time name"; GNU tar
    # prints "perms owner/group size date time name". Pick the size column from
    # whichever format this tar produced instead of assuming one layout.
    awk '{
      if ($2 ~ /^[0-9]+$/) size = $5; else size = $3
      if (size ~ /^[0-9]+$/) { count++; total += size }
    }
      END { if (count < 1 || count > limit || total > bytes) exit 1 }' \
      limit="$MAX_ARCHIVE_ENTRIES" bytes="$MAX_EXTRACTED_BYTES" "$verbose" ||
      die "The downloaded archive exceeds the installation limits."
  else
    zipinfo -1 "$WORK/$NAME" >"$paths" || die "Could not read the downloaded archive."
    zipinfo -l "$WORK/$NAME" >"$verbose" || die "Could not inspect the downloaded archive."
    unzip -l "$WORK/$NAME" >"$sizes" || die "Could not inspect the downloaded archive."
    awk '$1 ~ /^[0-9]+$/ && NF >= 4 { count++; total += $1 }
      END { if (count < 1 || count > limit || total > bytes) exit 1 }' \
      limit="$MAX_ARCHIVE_ENTRIES" bytes="$MAX_EXTRACTED_BYTES" "$sizes" ||
      die "The downloaded archive exceeds the installation limits."
  fi
  awk 'index($0, "/") == 1 || $0 ~ /(^|\/)\.\.(\/|$)/ { exit 1 }' "$paths" ||
    die "The downloaded archive contains an unsafe path."
  awk '$1 ~ /^[lh]/ { exit 1 }' "$verbose" ||
    die "The downloaded archive contains a link, which is not allowed."
}

copy_tree() {
  if command -v ditto >/dev/null 2>&1; then
    ditto "$1" "$2"
  else
    cp -R "$1" "$2"
  fi
}

# Return 0 when Calm Toad is running, 1 when it is not, and 2 when the answer
# cannot be determined safely. `pgrep` normally returns 1 for no match.
running_app() {
  if ! command -v pgrep >/dev/null 2>&1; then
    return 2
  fi
  pgrep -x calmtoad >/dev/null 2>&1
  app_status=$?
  if [ "$app_status" -eq 0 ]; then
    return 0
  fi
  if [ "$app_status" -gt 1 ]; then
    return 2
  fi
  pgrep -x 'Calm Toad' >/dev/null 2>&1
  app_status=$?
  if [ "$app_status" -eq 0 ]; then
    return 0
  fi
  if [ "$app_status" -gt 1 ]; then
    return 2
  fi
  return 1
}

check_not_running() {
  if running_app; then
    die "Calm Toad is running. Quit it first so no crawl or update is interrupted, then run this command again."
  else
    _running_status=$?
    [ "$_running_status" -eq 1 ] ||
      die "Could not determine whether Calm Toad is running. Install pgrep or quit Calm Toad and try again."
  fi
}

if [ "$PLATFORM" = "linux-x86_64" ]; then
  PREFIX="${PREFIX:-$HOME/.local/opt}"
  INSTALL_DIR="$PREFIX/calmtoad"
  STAGE="$INSTALL_DIR.new.$$"
  BACKUP="$INSTALL_DIR.backup.$$"
  say "Installing into $INSTALL_DIR"
  mkdir -p "$PREFIX" "$HOME/.local/bin"
  [ -w "$PREFIX" ] || die "$PREFIX is not writable. Choose another directory with --prefix."
  check_not_running
  recover_interrupted_install "$PREFIX" "$INSTALL_DIR" 'calmtoad.new.*' 'calmtoad.backup.*' 'Calm Toad/Calm Toad'
  if [ -e "$INSTALL_DIR" ] && [ ! -f "$INSTALL_DIR/Calm Toad/Calm Toad" ]; then
    die "$INSTALL_DIR already exists but is not a Calm Toad installation. Nothing was changed."
  fi
  assert_archive_safe
  mkdir "$STAGE"
  if ! tar -xzf "$WORK/$NAME" -C "$STAGE"; then
    rm -rf "$STAGE"
    die "Could not unpack the download."
  fi
  BINARY="$(find "$STAGE" -type f -name 'Calm Toad' | head -n 1)"
  if [ -z "$BINARY" ]; then
    rm -rf "$STAGE"
    die "The download did not contain the Calm Toad executable."
  fi
  chmod +x "$BINARY"

  transaction_target="$INSTALL_DIR"
  transaction_stage="$STAGE"
  transaction_backup="$BACKUP"
  transaction_started=1
  transaction_original_existed=0
  transaction_original_moved=0
  transaction_committed=0
  if [ -e "$INSTALL_DIR" ]; then
    transaction_original_existed=1
    mv "$INSTALL_DIR" "$BACKUP" || die "Could not move the existing installation aside."
    transaction_original_moved=1
    sync
  fi
  if mv "$STAGE" "$INSTALL_DIR"; then
    sync
    transaction_committed=1
  else
    die "Could not move the new installation into place."
  fi

  INSTALLED="$(find "$INSTALL_DIR" -type f -name 'Calm Toad' | head -n 1)"
  ln -sf "$INSTALLED" "$HOME/.local/bin/calmtoad"
  DESKTOP_EXEC="$(printf '%s' "$HOME/.local/bin/calmtoad" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e 's/`/\\`/g' -e 's/\$/\\$/g' -e 's/%/%%/g')"
  mkdir -p "$HOME/.local/share/applications"
  cat >"$HOME/.local/share/applications/calmtoad.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Calm Toad
Comment=Check a website for missing titles, broken links, and other common problems
Exec="$DESKTOP_EXEC"
Terminal=false
Categories=Development;Network;
DESKTOP

  printf '\n\033[32mCalm Toad %s is installed.\033[0m\n' "$RELEASE_VERSION"
  note "App:      $INSTALLED"
  note "Terminal: $HOME/.local/bin/calmtoad"
  case ":${PATH}:" in
    *":$HOME/.local/bin:"*) ;;
    *) note "Add ~/.local/bin to your PATH to run it from the terminal." ;;
  esac
  if [ "$LAUNCH" = "1" ]; then
    "$INSTALLED" >/dev/null 2>&1 &
  fi
  exit 0
fi

# macOS. ~/Applications needs no admin password and still appears in Launchpad
# and Spotlight, so a writable /Applications is preferred but never required.
if [ -z "$PREFIX" ]; then
  if [ -w /Applications ]; then
    PREFIX=/Applications
  else
    PREFIX="$HOME/Applications"
  fi
fi
TARGET="$PREFIX/Calm Toad.app"
STAGE="$PREFIX/.Calm Toad.app.new.$$"
BACKUP="$PREFIX/.Calm Toad.app.backup.$$"

say "Installing into $PREFIX"
mkdir -p "$PREFIX" || die "Cannot create $PREFIX. Re-run with --prefix ~/Applications"
[ -w "$PREFIX" ] || die "$PREFIX is not writable. Re-run with --prefix ~/Applications (no admin password needed)."
check_not_running
recover_interrupted_install "$PREFIX" "$TARGET" '.Calm Toad.app.new.*' '.Calm Toad.app.backup.*' 'Contents/MacOS/calmtoad'
if [ -e "$TARGET" ] && [ ! -f "$TARGET/Contents/MacOS/calmtoad" ]; then
  die "$TARGET already exists but is not a Calm Toad app. Nothing was changed."
fi

assert_archive_safe
if command -v ditto >/dev/null 2>&1; then
  ditto -x -k "$WORK/$NAME" "$WORK/unpacked" || die "Could not unpack the download."
else
  mkdir -p "$WORK/unpacked"
  unzip -q "$WORK/$NAME" -d "$WORK/unpacked" || die "Could not unpack the download."
fi
APP="$(find "$WORK/unpacked" -maxdepth 3 -type d -name 'Calm Toad.app' | head -n 1)"
[ -n "$APP" ] || die "The download did not contain Calm Toad.app."

# Stage beside the destination and finish all failure-prone work before moving
# the installed app. Renames within this directory stay on one filesystem.
if ! copy_tree "$APP" "$STAGE"; then
  rm -rf "$STAGE"
  die "Could not stage Calm Toad in $PREFIX. The existing app was not changed."
fi
chmod +x "$STAGE/Contents/MacOS/calmtoad" 2>/dev/null || true
if [ ! -x "$STAGE/Contents/MacOS/calmtoad" ]; then
  rm -rf "$STAGE"
  die "The downloaded app has no usable executable. The existing app was not changed."
fi
if ! xattr -dr com.apple.quarantine "$STAGE" 2>/dev/null; then
  rm -rf "$STAGE"
  die "macOS would not clear the quarantine flag. The existing app was not changed; try right-clicking Calm Toad and choosing Open after a manual install."
fi

transaction_target="$TARGET"
transaction_stage="$STAGE"
transaction_backup="$BACKUP"
transaction_started=1
transaction_original_existed=0
transaction_original_moved=0
transaction_committed=0
if [ -e "$TARGET" ]; then
  transaction_original_existed=1
  mv "$TARGET" "$BACKUP" || die "Could not move the existing app aside."
  transaction_original_moved=1
  sync
fi
if mv "$STAGE" "$TARGET"; then
  sync
  transaction_committed=1
else
  die "Could not move the new app into place."
fi

printf '\n\033[32mCalm Toad %s is installed.\033[0m\n' "$RELEASE_VERSION"
note "Location: $TARGET"
if [ "$LAUNCH" = "1" ]; then
  open "$TARGET" || die "Installed, but could not open it. Double-click Calm Toad in $PREFIX"
  note "Calm Toad is open. Next time, launch it from Launchpad or $PREFIX"
else
  note "Open it from Launchpad or with: open \"$TARGET\""
fi
