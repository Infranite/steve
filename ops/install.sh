#!/bin/zsh
# One-time macOS setup. Existing installs must preserve their labels and signing identity.
set -eu
HOME_DIR=${STEVE_HOME:-$HOME/.steve}
LABEL=${STEVE_LABEL:-io.github.infranite.steve}
BACKUP_LABEL=${STEVE_BACKUP_LABEL:-$LABEL-backup}
export STEVE_HOME=$HOME_DIR STEVE_LABEL=$LABEL
mkdir -p $HOME_DIR/{releases,web,data,worktrees,scratch,backups,logs/release,secrets} $HOME/Library/LaunchAgents
chmod 700 $HOME_DIR/secrets
[[ -f $HOME_DIR/steve.env ]] || { print "先写 $HOME_DIR/steve.env（见 ops/steve.env.example）"; exit 1 }
# plistlib handles spaces, XML characters and non-default installation locations.
python3 - "$HOME_DIR" "$LABEL" "$BACKUP_LABEL" "$HOME/Library/LaunchAgents" <<'PY'
import plistlib,sys
from pathlib import Path
home,label,backup,dest=sys.argv[1:]
for name,script,log in [(label,'run.sh','hub.log'),(backup,'backup.sh','backup.log')]:
    job={'Label':name,'ProgramArguments':['/bin/zsh',f'{home}/current/ops/{script}'],
         'EnvironmentVariables':{'STEVE_HOME':home},
         'StandardOutPath':f'{home}/logs/{log}','StandardErrorPath':f'{home}/logs/{log}'}
    if name==label: job.update(RunAtLoad=True,KeepAlive=True,ThrottleInterval=5)
    else: job['StartCalendarInterval']={'Hour':3,'Minute':0}
    with (Path(dest)/f'{name}.plist').open('wb') as f: plistlib.dump(job,f)
PY
if [[ ! -e $HOME_DIR/current ]]; then
  STEVE_FIRST=1 zsh ${0:A:h}/release.sh main
fi
for job in $LABEL $BACKUP_LABEL; do
  launchctl bootout gui/$(id -u)/$job 2>/dev/null || true
  launchctl bootstrap gui/$(id -u) $HOME/Library/LaunchAgents/$job.plist
done
print "installed; logs in $HOME_DIR/logs/hub.log"
