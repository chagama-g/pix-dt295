#!/bin/sh
# Before removal (deb: prerm remove; rpm: %preun 0), not before an upgrade: stops and
# disables dt295.service. The dt295 user and /var/lib/dt295 (the state files with the
# link keys) stay.
set -e

case "$1" in
    remove | 0) ;;
    *) exit 0 ;;
esac

if command -v systemctl >/dev/null 2>&1; then
    systemctl --no-reload disable dt295.service >/dev/null 2>&1 || :
    if [ -d /run/systemd/system ]; then
        systemctl stop dt295.service || :
    fi
fi
