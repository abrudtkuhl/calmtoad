#!/bin/sh
# Require MACOS_SIGN_IDENTITY to name a valid codesigning identity in the keychain.
# The identity list is inspected but never printed; no secrets are emitted.
set -eu

KEYCHAIN="${1:-}"

if [ -z "$KEYCHAIN" ]; then
  echo "A keychain path is required." >&2
  exit 1
fi
if ! command -v security >/dev/null 2>&1; then
  echo "macOS security tool is unavailable." >&2
  exit 1
fi
if [ -z "${MACOS_SIGN_IDENTITY:-}" ]; then
  echo "MACOS_SIGN_IDENTITY is empty; set it to the valid Developer ID Application identity." >&2
  exit 1
fi

if ! IDENTITIES="$(security find-identity -v -p codesigning "$KEYCHAIN" 2>&1)"; then
  echo "Could not list valid codesigning identities in the temporary keychain." >&2
  exit 1
fi

if ! printf '%s\n' "$IDENTITIES" | grep -Eq '[1-9][0-9]* valid identit(y|ies) found'; then
  echo "No valid codesigning identities found in the temporary keychain." >&2
  exit 1
fi

if ! printf '%s\n' "$IDENTITIES" | grep -Fq -- "$MACOS_SIGN_IDENTITY"; then
  echo "MACOS_SIGN_IDENTITY does not match any valid codesigning identity in the temporary keychain." >&2
  exit 1
fi
