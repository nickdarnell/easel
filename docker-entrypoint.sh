#!/bin/sh
# Runs the app as PUID:PGID (default 1000:1000) on PORT (default 3273), and makes sure it owns /config.
set -e
PUID=${PUID:-1000}
PGID=${PGID:-1000}
PORT=${PORT:-3273}
mkdir -p /config

# An explicit --port in the command wins over PORT.
case " $* " in
  *" --port "* | *" --port="*) ;;
  *) set -- --port "$PORT" "$@" ;;
esac

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
