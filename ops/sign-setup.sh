#!/bin/zsh
# One-time setup of the local code-signing identity "Steve Hub" that release.sh signs bin/steve-hub with.
#
# Why: the vault's master key lives in the login keychain, whose access control trusts the designated
# requirement of the binary that created it. Ad-hoc signed builds get a new requirement (cdhash) on every
# release, so after a release the Hub can no longer read the key. Signed with this identity and a fixed
# identifier, every release has the same requirement:
#   identifier "io.github.infranite.steve.hub" and certificate leaf = H"<this certificate>"
#
# The installer signs the Hub and helper with this identity, stores the random password in the
# installation's key store, and removes the private staging file after readback.
set -eu
source ${0:A:h}/release-lib.sh
HOME_DIR=${STEVE_HOME:?run steve setup to create the signing identity}
DIR=$HOME_DIR/signing KC=$HOME_DIR/signing/steve-signing.keychain-db NAME=${STEVE_SIGN_IDENTITY:-Steve Hub}
[[ -f $DIR/bundle-setup-started.json ]] || { print -u2 "run steve setup to create the signing identity"; exit 1; }
[[ ! -f $DIR/bundle-setup.json ]] || { print -u2 "signing setup is already complete"; exit 1; }
umask 077
STAGING=$DIR/.setup-password
if [[ ! -f $STAGING || -L $STAGING ]]; then
  [[ ! -e $KC ]] || { print -u2 "the signing keychain has no setup password"; exit 1; }
  (set -o noclobber; openssl rand -hex 32 >$STAGING)
fi
PASS=$(<$STAGING)
kc_quotable "$PASS" "$KC"
if [[ -f $KC ]] && kc_sign $KC "$PASS" security find-identity -p codesigning $KC | grep -q "\"$NAME\""; then
  exit 0
fi
TMP=$(mktemp -d)
trap 'rm -rf $TMP; [[ -f $KC ]] && security lock-keychain $KC 2>/dev/null || true' EXIT
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
