# Troubleshooting (Pi Zero W)

Logs: `journalctl -fu rascar-carplay` (set `RASCAR_DEBUG=1` in `/etc/rascar/carplay.env` for more,
`RASCAR_WIRE_TRACE=1` for hex dumps). Each attempt starts with the startup banner; the phase markers
(`Phase 1d START`, `ROLE-SWITCH RESULT`, `SessionReady`, `← RECORD OK`, `→ keyframe`) tell you how far
it got. To keep logs across the car's power cuts, make the journal persistent
(`sudo mkdir -p /var/log/journal` and `Storage=persistent` in `journald.conf`).

## Pi Zero specific

| Symptom | Cause | Fix |
|---|---|---|
| `gadget setup failed`, `no UDC available` | dwc2 not in OTG/peripheral mode | `dtoverlay=dwc2-role-switch` in `config.txt` (and no other `dtoverlay=dwc2…` line, no `modules-load=…g_ether` in `cmdline.txt`). `ls /sys/class/udc` must show `20980000.usb`. |
| Nothing at all in the log when plugged in, `/sys/class/udc/*/state` stays `not attached` | Cable plugged into `PWR IN`, or a charge-only cable | Use the port marked **USB** and a data cable. |
| `ROLE-SWITCH RESULT: method=none` | Kernel without `CONFIG_USB_ROLE_SWITCH`, or the overlay is missing `usb-role-switch` | Check `ls /sys/class/usb_role`. Otherwise set `RASCAR_ROLE_SWITCH_CMD=/usr/local/sbin/rascar-role-switch-rebind`. |
| Role switch OK but `waiting for car USB device` times out | dwc2 did not actually become host | `cat /sys/class/usb_role/*/role` should read `host` during the attempt; `dmesg` should show a new USB bus. Try the rebind fallback above. |
| `cdc_ncm … failed to get mac address`, no network interface after authentication | Stock `cdc_ncm` | `sudo kernel/cdc_ncm/build.sh`, reboot, `cat /sys/module/cdc_ncm/version` → `rascar-3`. |
| `ffmpeg exited … after 0 frame(s)` | No usable encoder or bad source | `ffmpeg -encoders | grep h264_v4l2m2m` and `ls /dev/video11`. Test the source by hand. Set `RASCAR_VIDEO_ENCODER` to force one. |
| Session dies right after `screen SETUP`, before `→ keyframe` | ffmpeg too slow to start on the Zero; the car gives up | `RASCAR_VIDEO_SOURCE=color:000000`, `RASCAR_VIDEO_FPS=10`, lower `RASCAR_VIDEO_BITRATE_KBPS`. |
| Pi reboots or the USB link resets when the car connects | The car's port cannot power the Pi through the role switch | Power the Pi from `PWR IN` with its own 5 V supply. |

## CarPlay protocol (from the original car tests)

| Symptom | Cause | Fix |
|---|---|---|
| Car configures the Pi (`C0 53` answered) but never sends the role switch `40 51` | Normal for a cold start: the head unit decides 22-27 s after it powers up | Wait. The gadget is re-presented after `RASCAR_ROLE_SWITCH_GRACE_MS` (45 s). Do not shorten it much: re-presenting inside the car's decision window wedged it. |
| The car never sends the role switch again, even across reboots | The car's CarPlay stack is wedged, or it remembers this "iPhone" | Let the car go to sleep and start it again. Delete `/var/lib/rascar/usb_serial` to present a new iPhone. |
| Session dies 4-6 s after the role switch, around authentication | USB2 Link Power Management | The code writes the usbcore quirk `8086:9999:ak`. A different head unit has a different VID:PID: add it (`host/role_switch.rs`). |
| NCM bring-up stalls; interface renamed late or loses its addresses | udev renaming / NetworkManager managing the car link | `pizero/install.sh` installs `10-rascar-ncm.link` and the NetworkManager `unmanaged-devices=driver:cdc_ncm` drop-in. |
| Connected, but the car stays on its own UI until you press the CarPlay icon | The car's `/info` says its UI takes the main screen | Handled by the initial screen sync (screen 2/2 then 1/1); `RASCAR_INITIAL_SCREEN_SYNC=off` disables it. |
| `usb usbN-port1: disabled by hub (EMI?)`, `error -71` on the car link | Signal problem on the cable | Shorter, better cable; no extension or hub. |

[protocol.md](protocol.md) and [reverse-engineering-notes.md](reverse-engineering-notes.md) explain the
mechanisms behind these.
