#!/bin/sh
# After install or upgrade (deb: postinst configure [<old version>]; rpm: %post 1 or 2):
# what pacman's hooks do for the Arch package. The dt295 user and /var/lib/dt295, the
# udev rule for a unit already plugged in, and systemd's view of dt295.service. A
# running daemon is not restarted: a recording may be in progress (INSTALL.md).
set -e

case "$1" in
    configure | 1 | 2) ;;
    *) exit 0 ;;
esac

# systemd-sysusers / systemd-tmpfiles are missing in a container without systemd.
if command -v systemd-sysusers >/dev/null 2>&1; then
    systemd-sysusers dt295.conf
elif ! getent passwd dt295 >/dev/null; then
    useradd --system --user-group --home-dir /var/lib/dt295 --shell /usr/sbin/nologin \
        --comment "PIX-DT295 tuner daemon" dt295
fi
if command -v systemd-tmpfiles >/dev/null 2>&1; then
    systemd-tmpfiles --create dt295.conf
else
    install -d -m 0750 -o dt295 -g dt295 /var/lib/dt295
fi

# Only with udevd running: a container (LXC) has no /run/udev/control.
if command -v udevadm >/dev/null 2>&1 && [ -S /run/udev/control ]; then
    udevadm control --reload-rules || :
    udevadm trigger --subsystem-match=usb --attr-match=idVendor=06b8 || :
fi
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || :
fi

# A first install: deb passes no old version, rpm passes 1.
if [ "$1" = 1 ] || { [ "$1" = configure ] && [ -z "$2" ]; }; then
    cat <<'EOF'
==> Put the vendor firmware (mb86m01_assp_osec_idle.bin and mb86m01_assp_osec_trc_m_h.bin)
    in /usr/lib/firmware/dt295/ and list the units in /etc/dt295.conf ([unit <serial>],
    the serial from `dt295 list`), then: systemctl enable --now dt295
EOF
fi
