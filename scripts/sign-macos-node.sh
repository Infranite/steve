#!/bin/bash
# Run on the isolated macOS release runner before archiving the binaries.
set -euo pipefail

: "${STEVE_MACOS_SIGNING_P12:?configure the release Developer ID Application certificate}"
: "${STEVE_MACOS_SIGNING_PASSWORD:?configure the release certificate password}"
: "${STEVE_MACOS_SIGNING_TEAM:?configure the stable release signing Team ID}"
[[ $STEVE_MACOS_SIGNING_TEAM =~ ^[A-Z0-9]{10}$ ]]
[[ $# == 2 && -f $1 && -f $2 ]]

credentials=$(mktemp -d)
keychain="$credentials/signing.keychain-db"
certificate="$credentials/signing.p12"
trap 'security delete-keychain "$keychain" >/dev/null 2>&1 || true; rm -f "$certificate"; rmdir "$credentials"' EXIT
umask 077
printf '%s' "$STEVE_MACOS_SIGNING_P12" | /usr/bin/base64 -D > "$certificate"
keychain_password=$(/usr/bin/openssl rand -hex 32)
security create-keychain -p "$keychain_password" "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
security import "$certificate" -k "$keychain" -P "$STEVE_MACOS_SIGNING_PASSWORD" -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple: -s -k "$keychain_password" "$keychain" >/dev/null
identity=$(security find-identity -v -p codesigning "$keychain" |
  awk -v team="($STEVE_MACOS_SIGNING_TEAM)" '/"Developer ID Application:/ && index($0, team) { print $2 }')
[[ $identity =~ ^[A-F0-9]{40}$ ]]

sign() {
  local binary=$1 identifier=$2
  local requirement="identifier \"$identifier\" and anchor apple generic and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"$STEVE_MACOS_SIGNING_TEAM\""
  /usr/bin/codesign --force --sign "$identity" --keychain "$keychain" \
    --identifier "$identifier" --options runtime --timestamp "$binary"
  /usr/bin/codesign --verify --strict -R "$requirement" "$binary"
}

sign "$1" io.github.infranite.steve.cli
sign "$2" io.github.infranite.steve.node
