# The body of post_install in the Arch package's .INSTALL (nfpm adds the function
# around it): the note of packaging/arch/pix-dt295.install.
cat <<'EOF'
==> Put the vendor firmware (mb86m01_assp_osec_idle.bin and mb86m01_assp_osec_trc_m_h.bin)
    in /usr/lib/firmware/dt295/ and list the units in /etc/dt295.conf ([unit <serial>],
    the serial from `dt295 list`), then: systemctl enable --now dt295
EOF
