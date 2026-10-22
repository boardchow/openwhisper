#!/usr/bin/env bash
# Creates a stable self-signed code-signing identity for OpenWhisper in a dedicated
# keychain. Signing with a stable identity (instead of ad-hoc "-") keeps the
# app's code signature constant across rebuilds, so macOS TCC permissions
# (Accessibility, Input Monitoring, Microphone) persist and don't need
# re-granting after every build.
#
# Run once:  ./build/setup-signing.sh
set -euo pipefail

IDENTITY="OpenWhisper Local Dev"
KEYCHAIN="openwhisper-signing.keychain-db"
KEYCHAIN_PASS="openwhisper"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Idempotent: if the identity already exists in our keychain, do nothing. (Check
# the keychain directly, not `find-identity -v`, since a self-signed cert is not
# "valid" and would never match — which would regenerate it every run and reset
# all TCC permissions.)
if security find-identity "$KEYCHAIN" 2>/dev/null | grep -q "$IDENTITY"; then
	echo ">> identity '$IDENTITY' already exists; nothing to do"
	exit 0
fi

echo ">> generating self-signed code-signing certificate"
cat >"$WORK/openssl.cnf" <<'EOF'
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = OpenWhisper Local Dev
[ext]
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
basicConstraints = critical, CA:false
EOF

openssl req -x509 -newkey rsa:2048 -nodes \
	-keyout "$WORK/key.pem" -out "$WORK/cert.pem" \
	-days 3650 -config "$WORK/openssl.cnf"

# -legacy: Apple's Security framework can't verify OpenSSL 3's default PKCS12
# MAC, so export with the legacy (SHA1/3DES) algorithms it understands.
openssl pkcs12 -export -out "$WORK/identity.p12" -legacy \
	-inkey "$WORK/key.pem" -in "$WORK/cert.pem" -passout pass:openwhisper

echo ">> creating signing keychain"
# Recreate cleanly if a stale one exists.
security delete-keychain "$KEYCHAIN" 2>/dev/null || true
security create-keychain -p "$KEYCHAIN_PASS" "$KEYCHAIN"
security set-keychain-settings "$KEYCHAIN" # disable auto-lock timeout
security unlock-keychain -p "$KEYCHAIN_PASS" "$KEYCHAIN"

echo ">> importing identity"
security import "$WORK/identity.p12" -k "$KEYCHAIN" -P "openwhisper" -T /usr/bin/codesign -A
# Allow codesign to use the key without an interactive prompt.
security set-key-partition-list -S apple-tool:,apple:,codesign: \
	-s -k "$KEYCHAIN_PASS" "$KEYCHAIN" >/dev/null

echo ">> adding keychain to the search list"
EXISTING=$(security list-keychains -d user | sed 's/[",]//g' | xargs)
# shellcheck disable=SC2086
security list-keychains -d user -s "$KEYCHAIN" $EXISTING

echo ">> done. Sign with: codesign --sign \"$IDENTITY\" --keychain $KEYCHAIN ..."
security find-identity -v -p codesigning | grep "$IDENTITY" || true
