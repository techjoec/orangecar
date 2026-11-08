#!/bin/bash
# role-switch-rebind.sh host|device — fallback role switch for kernels without
# CONFIG_USB_ROLE_SWITCH. Enable it with, in /etc/rascar/carplay.env:
#   RASCAR_ROLE_SWITCH_CMD=/usr/local/sbin/rascar-role-switch-rebind
# It unbinds dwc2, applies (or removes) the dwc2-host runtime overlay, and binds
# dwc2 again so the controller re-probes in the new mode.
set -eu
CTRL=${RASCAR_UDC:-20980000.usb}
DRV=/sys/bus/platform/drivers/dwc2

unbind() { [[ -e $DRV/$CTRL ]] && echo "$CTRL" > "$DRV/unbind" || true; }
bind()   { echo "$CTRL" > "$DRV/bind"; }

case ${1:-} in
    host)
        unbind
        dtoverlay -r dwc2-host 2>/dev/null || true
        dtoverlay dwc2-host
        bind
        ;;
    device)
        unbind
        dtoverlay -r dwc2-host 2>/dev/null || true
        bind
        ;;
    *) echo "usage: $0 host|device" >&2; exit 2 ;;
esac
