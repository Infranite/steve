#!/bin/zsh
# daily: online SQLite backup, keep 14 days
HOME_DIR=${STEVE_HOME:-$HOME/.steve}; mkdir -p $HOME_DIR/backups
sqlite3 $HOME_DIR/data/steve.sqlite3 ".backup '$HOME_DIR/backups/daily-$(date +%Y%m%d).sqlite3'" && find $HOME_DIR/backups -name 'daily-*.sqlite3' -mtime +14 -delete
