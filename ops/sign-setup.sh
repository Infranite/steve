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
# with a random password written next to it (0600), so signing never needs the user's login password. The Hub
# then moves that password into the login keychain through steve-keyhelper (only the signed Hub reads it there
# unasked; agents can read files) and removes the file. The keychain stays locked except around a codesign
# (ops/release-lib.sh kc_sign), and its password never goes on a command line.
# The certificate is self-signed and not trusted system-wide; codesign signs with it anyway and the keychain
# matches on the certificate hash, not on trust. Idempotent: does nothing if the identity exists.
set -eu
source ${0:A:h}/release-lib.sh   # kc_* and sign_pass
HOME_DIR=${STEVE_HOME:-$HOME/.steve}
DIR=$HOME_DIR/signing KC=$HOME_DIR/signing/steve-signing.keychain-db NAME=${STEVE_SIGN_IDENTITY:-Steve Hub}
mkdir -p $DIR && chmod 700 $DIR
PASS=""
if [[ -f $KC ]]; then
  # signing/keychain-pass, or the login keychain entry the Hub moved it into (a macOS prompt, from a terminal only)
  PASS=$(sign_pass $KC) || { print -u2 -r -- "$KC exists but its password did not open"; exit 1 }
  if kc_sign $KC "$PASS" security find-identity -p codesigning $KC | grep -q "\"$NAME\""; then
    print -r -- "signing identity \"$NAME\" already in $KC"; exit 0
  fi
fi
umask 077
TMP=$(mktemp -d)
# whatever happens from here, the temporary key goes and the keychain is locked again
trap 'rm -rf $TMP; [[ -f $KC ]] && security lock-keychain $KC 2>/dev/null || true' EXIT
if [[ -z $PASS ]]; then
  [[ -f $DIR/keychain-pass ]] || openssl rand -hex 32 >$DIR/keychain-pass
  PASS=$(<$DIR/keychain-pass)
fi
kc_quotable "$PASS" "$KC"
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
# the p12's password reaches openssl through its environment (env:), security through stdin: no command line has it
P12=$P12 openssl pkcs12 -export -legacy -inkey $TMP/key.pem -in $TMP/cert.pem -out $TMP/id.p12 -passout env:P12 2>/dev/null \
  || P12=$P12 openssl pkcs12 -export -inkey $TMP/key.pem -in $TMP/cert.pem -out $TMP/id.p12 -passout env:P12
[[ -f $KC ]] || kc_security "create-keychain -p $PASS \"$KC\""
kc_unlock $KC "$PASS"   # and from now on it locks itself after 5 idle minutes and on sleep
kc_security "import \"$TMP/id.p12\" -k \"$KC\" -P $P12 -T /usr/bin/codesign" >/dev/null
kc_security "set-key-partition-list -S apple-tool:,apple: -s -k $PASS \"$KC\"" >/dev/null
security find-identity -p codesigning $KC | grep "\"$NAME\""
print -r -- "created signing identity \"$NAME\" in $KC"
