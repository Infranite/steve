#!/bin/zsh
# One-time setup of the local code-signing identity "Steve Hub" that release.sh signs bin/steve-hub with.
#
# Why: the vault's master key lives in the login keychain, whose access control trusts the designated
# requirement of the binary that created it. Ad-hoc signed builds get a new requirement (cdhash) on every
# release, so after a release the Hub can no longer read the key. Signed with this identity and a fixed
# identifier, every release has the same requirement:
#   identifier "io.github.infranite.steve.hub" and certificate leaf = H"<this certificate>"
#
# The identity sits in its own keychain file under $STEVE_HOME/signing (not the login keychain), unlocked
# with a random password kept next to it (0600), so signing never needs the user's login password.
# The certificate is self-signed and not trusted system-wide; codesign signs with it anyway and the keychain
# matches on the certificate hash, not on trust. Idempotent: does nothing if the identity exists.
set -eu
HOME_DIR=${STEVE_HOME:-$HOME/.steve}
DIR=$HOME_DIR/signing KC=$HOME_DIR/signing/steve-signing.keychain-db NAME=${STEVE_SIGN_IDENTITY:-Steve Hub}
mkdir -p $DIR && chmod 700 $DIR
if [[ -f $KC && -f $DIR/keychain-pass ]]; then
  security unlock-keychain -p "$(<$DIR/keychain-pass)" $KC
  if security find-identity -p codesigning $KC | grep -q "\"$NAME\""; then
    print -r -- "signing identity \"$NAME\" already in $KC"; exit 0
  fi
fi
umask 077
TMP=$(mktemp -d); trap 'rm -rf $TMP' EXIT
[[ -f $DIR/keychain-pass ]] || openssl rand -hex 32 >$DIR/keychain-pass
PASS=$(<$DIR/keychain-pass)
cat >$TMP/cert.cnf <<EOF
[req]
distinguished_name=dn
x509_extensions=v3
prompt=no
[dn]
CN=$NAME
[v3]
basicConstraints=critical,CA:false
keyUsage=critical,digitalSignature
extendedKeyUsage=critical,codeSigning
EOF
openssl req -x509 -newkey rsa:3072 -nodes -keyout $TMP/key.pem -out $TMP/cert.pem -days 36500 -config $TMP/cert.cnf 2>/dev/null
P12=$(openssl rand -hex 16)
openssl pkcs12 -export -legacy -inkey $TMP/key.pem -in $TMP/cert.pem -out $TMP/id.p12 -passout pass:$P12 2>/dev/null \
  || openssl pkcs12 -export -inkey $TMP/key.pem -in $TMP/cert.pem -out $TMP/id.p12 -passout pass:$P12
[[ -f $KC ]] || security create-keychain -p "$PASS" $KC
security unlock-keychain -p "$PASS" $KC
security set-keychain-settings $KC   # no auto-lock timeout
security import $TMP/id.p12 -k $KC -P $P12 -T /usr/bin/codesign >/dev/null
security set-key-partition-list -S apple-tool:,apple: -s -k "$PASS" $KC >/dev/null
security find-identity -p codesigning $KC | grep "\"$NAME\""
print -r -- "created signing identity \"$NAME\" in $KC"
