#!/bin/bash
#
# install.sh — set up a Raspberry Pi Zero W (Raspberry Pi OS Lite, 32-bit) as a
# minimal CarPlay "iPhone".
#
#   sudo pizero/install.sh [path/to/rascar-carplay] [--no-ncm]
#
# The binary is cross-compiled on a PC (see README.md); without a path the
# script looks in target/arm-unknown-linux-gnueabihf/release/.
# --no-ncm skips building the patched cdc_ncm module (do it later with
# kernel/cdc_ncm/build.sh).
#
# Reboot afterwards.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(dirname "$HERE")"
BIN=""
BUILD_NCM=1
for a in "$@"; do
    case $a in
        --no-ncm) BUILD_NCM=0 ;;
        -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
        *) BIN=$a ;;
    esac
done
BIN=${BIN:-$REPO/target/arm-unknown-linux-gnueabihf/release/rascar-carplay}

log() { echo "[pizero] $*"; }
die() { echo "[pizero] ERROR: $*" >&2; exit 1; }

[[ $EUID -eq 0 ]] || die "run as root"
[[ -f $BIN ]] || die "no binary at $BIN — cross-compile it first (README.md) or pass its path"
if [[ $(uname -m) != armv6l ]]; then
    log "note: this is $(uname -m), not a Pi Zero (armv6l); continuing anyway"
fi

BOOT=/boot/firmware
[[ -d $BOOT/overlays ]] || BOOT=/boot
[[ -d $BOOT/overlays ]] || die "cannot find the boot partition (/boot/firmware or /boot)"

# --- 1. packages -------------------------------------------------------------
log "installing packages"
apt-get update
apt-get install -y --no-install-recommends \
    ffmpeg avahi-daemon avahi-utils iproute2 iputils-ping kmod device-tree-compiler
if (( BUILD_NCM )); then
    apt-get install -y --no-install-recommends build-essential patch curl linux-headers-rpi-v6 \
        || log "could not install the kernel headers; build cdc_ncm later with kernel/cdc_ncm/build.sh"
fi

# --- 2. dwc2 in OTG mode with a software role switch -------------------------
log "building device-tree overlays into $BOOT/overlays"
dtc -@ -q -I dts -O dtb -o "$BOOT/overlays/dwc2-role-switch.dtbo" "$HERE/overlays/dwc2-role-switch.dts"
dtc -@ -q -I dts -O dtb -o "$BOOT/overlays/dwc2-host.dtbo" "$HERE/overlays/dwc2-host.dts"

CONFIG=$BOOT/config.txt
cp -n "$CONFIG" "$CONFIG.pre-rascar" || true
# Drop any other dwc2/otg overlay, then enable ours once, in the [all] section.
sed -i -E '/^\s*dtoverlay=dwc2(,.*)?\s*$/d; /^\s*dtoverlay=dwc2-role-switch\s*$/d; /^\s*otg_mode=1\s*$/d' "$CONFIG"
printf '\n[all]\n# rascar-carplay: dwc2 OTG with a software USB role switch\ndtoverlay=dwc2-role-switch\n' >> "$CONFIG"
log "config.txt: dtoverlay=dwc2-role-switch (backup in $CONFIG.pre-rascar)"

CMDLINE=$BOOT/cmdline.txt
if grep -qE 'modules-load=[^ ]*g_(ether|serial|mass_storage|multi)' "$CMDLINE" 2>/dev/null; then
    log "WARNING: $CMDLINE loads a legacy USB gadget (modules-load=...g_*); remove it, it would hold the UDC"
fi

cat > /etc/modules-load.d/rascar.conf <<'MODS'
# rascar-carplay builds its Apple-device gadget in configfs.
libcomposite
usb_f_fs
MODS

# --- 3. networking: leave the car's NCM link to rascar-carplay ---------------
install -D -m 0644 "$HERE/10-rascar-ncm.link" /etc/systemd/network/10-rascar-ncm.link
if [[ -d /etc/NetworkManager/conf.d ]]; then
    install -D -m 0644 "$HERE/10-rascar-ncm-unmanaged.conf" /etc/NetworkManager/conf.d/10-rascar-ncm-unmanaged.conf
fi

# --- 4. binary, scripts, service ---------------------------------------------
install -D -m 0755 "$BIN" /usr/local/bin/rascar-carplay
install -D -m 0755 "$HERE/rascar-carplay-run" /usr/local/sbin/rascar-carplay-run
install -D -m 0755 "$HERE/role-switch-rebind.sh" /usr/local/sbin/rascar-role-switch-rebind
install -d -m 0755 /etc/rascar /var/lib/rascar
[[ -f /etc/rascar/carplay.env ]] || install -m 0644 "$HERE/carplay.env.example" /etc/rascar/carplay.env
install -D -m 0644 "$HERE/rascar-carplay.service" /etc/systemd/system/rascar-carplay.service
systemctl daemon-reload
systemctl enable avahi-daemon rascar-carplay

# --- 5. patched cdc_ncm --------------------------------------------------------
if (( BUILD_NCM )); then
    log "building the patched cdc_ncm module (several minutes on a Pi Zero)"
    "$REPO/kernel/cdc_ncm/build.sh" || log "cdc_ncm build failed — see kernel/README.md; the car's NCM link needs it"
fi

log "done. Reboot, then check:  ls /sys/class/usb_role   (expect 20980000.usb-role-switch)"
log "logs:  journalctl -fu rascar-carplay"
