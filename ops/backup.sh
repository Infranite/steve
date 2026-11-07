#!/bin/zsh
# daily, keep 14 days:
# - an online SQLite backup, checked (quick_check) before it is kept;
# - what the database alone can't bring back, in one tar readable by the owner only: attachments
#   (data/files), the signing identity a Hub must be signed with to read the vault's master key
#   (signing, without the keychain's password: the Hub keeps that in the login keychain, and a copy in
#   a file would unlock the identity for anyone who reads the backup), the secret files the
#   configuration points at (secrets) and the configuration (steve.env).
# Same disk, no restore drill: a copy against mistakes and a corrupt database, not disaster recovery.
setopt err_return pipe_fail
HOME_DIR=${STEVE_HOME:-$HOME/.steve}
OUT=$HOME_DIR/backups
DAY=$(date +%Y%m%d)
umask 077
mkdir -p $OUT && chmod 700 $OUT

db=$OUT/daily-$DAY.sqlite3
tmp=$db.partial
rm -f $tmp
sqlite3 $HOME_DIR/data/steve.sqlite3 ".backup '$tmp'"
if [[ $(sqlite3 $tmp 'PRAGMA quick_check') != ok ]]; then
  print -u2 "backup: $tmp failed quick_check, not kept"
  rm -f $tmp
  return 1
fi
mv -f $tmp $db

parts=()
for p in data/files signing secrets steve.env; do
  [[ -e $HOME_DIR/$p ]] && parts+=($p)
done
if (( ${#parts} )); then
  tar -C $HOME_DIR --exclude signing/keychain-pass -cf $OUT/daily-$DAY-files.tar.partial $parts
  mv -f $OUT/daily-$DAY-files.tar.partial $OUT/daily-$DAY-files.tar
fi

find $OUT -name 'daily-*.sqlite3' -mtime +14 -delete
find $OUT -name 'daily-*-files.tar' -mtime +14 -delete
