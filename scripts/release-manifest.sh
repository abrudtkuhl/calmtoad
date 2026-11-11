#!/usr/bin/env bash
# Build the updater manifest from the release assets.
#
# Usage: scripts/release-manifest.sh <version> <tag> <notes-file> <dist-dir>
#
# The manifest is not signed here; the release workflow signs the file with
# `minisign -Sm` after this script writes it. The version must be stable: the
# app does not install prereleases or downgrades.
set -euo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: $0 <version> <tag> <notes-file> <dist-dir>" >&2
  exit 2
fi

version="$1"
tag="$2"
notes_file="$3"
dist="$4"
repo="${GITHUB_REPOSITORY:-abrudtkuhl/calmtoad}"
out="$dist/latest.json"

if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "The updater only ships stable versions; got: $version" >&2
  exit 1
fi
if [[ ! -s "$notes_file" ]]; then
  echo "Release notes are missing or empty: $notes_file" >&2
  exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required to build the update manifest." >&2
  exit 1
fi

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    echo "No SHA-256 tool (sha256sum or shasum) is available." >&2
    exit 1
  fi
}

size_of() {
  wc -c <"$1" | tr -d '[:space:]'
}

# One short line for the in-app banner; the full notes stay on the release page.
summary="$(grep -m1 -E '^[-*+] +[^ ]' "$notes_file" | sed -E 's/^[-*+] +//' || true)"
if [[ -z "$summary" ]]; then
  summary="See the release notes on GitHub."
fi

asset() {
  local key="$1" name="$2" file="$dist/$2"
  if [[ ! -f "$file" ]]; then
    echo "Missing release asset for $key: $file" >&2
    exit 1
  fi
  jq -n \
    --arg url "https://github.com/$repo/releases/download/$tag/$name" \
    --arg sha256 "$(sha256_of "$file")" \
    --argjson size "$(size_of "$file")" \
    '{ url: $url, sha256: $sha256, size: $size }'
}

jq -n \
  --argjson schema 1 \
  --arg version "$version" \
  --arg published_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg release_url "https://github.com/$repo/releases/tag/$tag" \
  --arg notes "$summary" \
  --argjson macos_aarch64 "$(asset macos-aarch64 Calm-Toad-Mac-Apple-Silicon.zip)" \
  --argjson macos_x86_64 "$(asset macos-x86_64 Calm-Toad-Mac-Intel.zip)" \
  --argjson windows_x86_64 "$(asset windows-x86_64 Calm-Toad-Windows.zip)" \
  --argjson linux_x86_64 "$(asset linux-x86_64 Calm-Toad-Linux.tar.gz)" \
  '{
    schema_version: $schema,
    version: $version,
    published_at: $published_at,
    release_url: $release_url,
    notes: $notes,
    platforms: {
      "macos-aarch64": $macos_aarch64,
      "macos-x86_64": $macos_x86_64,
      "windows-x86_64": $windows_x86_64,
      "linux-x86_64": $linux_x86_64
    }
  }' >"$out"

echo "Wrote $out"
