#!/bin/sh
# Runs the app as PUID:PGID (default 1000:1000) and makes sure it owns /config.
set -e
PUID=${PUID:-1000}
PGID=${PGID:-1000}
mkdir -p /config

# Started with --user (or already unprivileged): nothing to switch.
if [ "$(id -u)" != "0" ]; then
  exec frametv serve "$@"
fi

if [ "$(stat -c %u:%g /config)" != "$PUID:$PGID" ]; then
  echo "Giving /config to $PUID:$PGID"
  chown -R "$PUID:$PGID" /config
fi
[ "$PUID" = "0" ] && exec frametv serve "$@"
exec setpriv --reuid="$PUID" --regid="$PGID" --clear-groups frametv serve "$@"
