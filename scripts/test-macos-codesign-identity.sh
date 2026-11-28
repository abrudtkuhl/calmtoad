#!/usr/bin/env bash
# Deterministic, offline test for the release signing identity check.
# Uses a mock security tool and dummy values; no real keychain or secrets.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/calmtoad-signing-test.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/bin"
cat >"$WORK/bin/security" <<'SH'
#!/bin/sh
printf '%s\n' "${1:-}" >>"$CALL_LOG"
if [ "${1:-}" = "find-identity" ]; then
  printf '%s\n' "${MOCK_FIND_IDENTITY_OUTPUT:-}"
  exit "${MOCK_FIND_STATUS:-0}"
fi
exit 0
SH
chmod +x "$WORK/bin/security"
export CALL_LOG="$WORK/calls"
export PATH="$WORK/bin:$PATH"

IDENTITY='Developer ID Application: Fixture (TEAMID)'
DUMMY_PASSWORD='not-a-secret-test-value'
VALID_OUTPUT='1) 0123456789ABCDEF "Developer ID Application: Fixture (TEAMID)"
1 valid identity found'

: >"$CALL_LOG"
output="$(MACOS_SIGN_IDENTITY="$IDENTITY" MACOS_CERT_PASSWORD="$DUMMY_PASSWORD" \
  MOCK_FIND_IDENTITY_OUTPUT="$VALID_OUTPUT" \
  "$ROOT/scripts/check-macos-codesign-identity.sh" "$WORK/keychain" 2>&1)"
grep -qx 'find-identity' "$CALL_LOG"
case "$output" in
  *not-a-secret-test-value*) echo 'identity check printed a secret' >&2; exit 1 ;;
esac

: >"$CALL_LOG"
if output="$(MACOS_SIGN_IDENTITY="$IDENTITY" MACOS_CERT_PASSWORD="$DUMMY_PASSWORD" \
  MOCK_FIND_IDENTITY_OUTPUT='0 valid identities found' \
  "$ROOT/scripts/check-macos-codesign-identity.sh" "$WORK/keychain" 2>&1)"; then
  echo 'zero valid identities unexpectedly passed' >&2
  exit 1
fi
printf '%s\n' "$output" | grep -q 'No valid codesigning identities found'
case "$output" in
  *not-a-secret-test-value*) echo 'identity check printed a secret' >&2; exit 1 ;;
esac

if output="$(MACOS_SIGN_IDENTITY="$IDENTITY" MACOS_CERT_PASSWORD="$DUMMY_PASSWORD" \
  MOCK_FIND_IDENTITY_OUTPUT='1) 0123456789ABCDEF "Developer ID Application: Other (OTHER)"
1 valid identity found' \
  "$ROOT/scripts/check-macos-codesign-identity.sh" "$WORK/keychain" 2>&1)"; then
  echo 'non-matching identity unexpectedly passed' >&2
  exit 1
fi
printf '%s\n' "$output" | grep -q 'MACOS_SIGN_IDENTITY does not match'
case "$output" in
  *not-a-secret-test-value*) echo 'identity check printed a secret' >&2; exit 1 ;;
esac

if output="$(MACOS_SIGN_IDENTITY="$IDENTITY" MACOS_CERT_PASSWORD="$DUMMY_PASSWORD" \
  MOCK_FIND_STATUS=1 \
  "$ROOT/scripts/check-macos-codesign-identity.sh" "$WORK/keychain" 2>&1)"; then
  echo 'security find-identity failure unexpectedly passed' >&2
  exit 1
fi
printf '%s\n' "$output" | grep -q 'Could not list valid codesigning identities'

# PKCS#12 must import as an aggregate, not certificate-only.
grep -Fq -- '-t agg -f pkcs12' "$ROOT/.github/workflows/release.yml"
! grep -Fq -- '-t cert -f pkcs12' "$ROOT/.github/workflows/release.yml"

echo 'macOS codesigning identity checks passed'
