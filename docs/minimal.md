# Minimal profile: the iPhone side only (Raspberry Pi Zero W)

Sometimes you only need the car to believe a CarPlay iPhone is connected, without the dashboard, maps,
music, OBD-II or 4G. The **minimal profile** runs `rascar-carplay` alone, with the same protocol code
as the full build, on a board as small as a **Raspberry Pi Zero W**:

- the board enumerates as an Apple iPhone, answers the car's role switch and becomes USB host;
- it runs the phone side of iAP2 and brings up the CDC-NCM link;
- it opens the CarPlay session and keeps it alive;
- the car's screen shows either **nothing from us** (the car keeps its own UI) or a **still picture**
  (a colour, an image, a test card) that is encoded once and replayed, so almost no CPU is used;
- car touches and steering-wheel keys come back as UDP text lines, for anything you want to drive.

No `rascar-app`, no Chromium, no audio, no Now Playing. All of this is configuration of the normal
`rascar-carplay` binary: with none of the variables below set, it behaves exactly as the full build.

> **Status:** the protocol path is the one that runs in the reference car. The Pi-specific parts
> (the dwc2 role switch and the VideoCore H.264 encoder) and the no-video mode have passed the bench
> simulator but have **not been tested in a car yet**. Reports welcome.

## Hardware

- **Raspberry Pi Zero W** (or Zero / Zero 2 W; any Pi whose OTG port is driven by `dwc2`).
- A micro-SD card with **Raspberry Pi OS Lite (Bookworm, 32-bit)**.
- A **micro-USB data cable** from the Pi's **USB** port (not `PWR IN`) to the car's CarPlay port. On
  the Zero the data port's VBUS feeds the 5 V rail, so the car powers the Pi. Optionally power
  `PWR IN` separately, so the Pi is booted (~30 s) before the car looks.

## Build (on a PC)

The Pi Zero W is ARMv6: use the `arm-unknown-linux-gnueabihf` target (an armv7 binary crashes on a
Zero). Only `rascar-carplay` is needed.

```bash
rustup target add arm-unknown-linux-gnueabihf
pip install ziglang && cargo install cargo-zigbuild
cargo zigbuild --release --target arm-unknown-linux-gnueabihf.2.31 -p rascar-carplay
```

## Install (on the Pi)

Copy the repository and the binary to the Pi, then:

```bash
sudo pizero/install.sh target/arm-unknown-linux-gnueabihf/release/rascar-carplay
sudo reboot
```

`pizero/install.sh`:

1. installs `ffmpeg`, `avahi-daemon`/`avahi-utils`, `iproute2` and the kernel headers;
2. builds `pizero/overlays/dwc2-role-switch.dts` and sets `dtoverlay=dwc2-role-switch` in
   `config.txt`: `dwc2` in OTG mode **with a software USB role switch**, which `rascar-carplay`
   flips through `/sys/class/usb_role` when the car asks the "iPhone" to become host;
3. loads `libcomposite` and `usb_f_fs`; keeps udev, networkd and NetworkManager off the car link;
4. installs `/usr/local/bin/rascar-carplay`, `rascar-carplay-minimal.service` and the profile
   `pizero/carplay-minimal.env` as `/etc/rascar/carplay.env`;
5. builds the patched `cdc_ncm` module (`kernel/cdc_ncm/`, several minutes on a Zero).

Check after the reboot:

```bash
ls /sys/class/usb_role                 # 20980000.usb-role-switch
cat /sys/module/cdc_ncm/version        # rascar-3
journalctl -fu rascar-carplay-minimal  # then plug into the car
```

If `/sys/class/usb_role` is empty, the kernel lacks `CONFIG_USB_ROLE_SWITCH`: set
`RASCAR_ROLE_SWITCH_CMD=/usr/local/sbin/rascar-role-switch-rebind` (unbinds `dwc2`, applies a
`dr_mode=host` runtime overlay, binds it again; slower and less tested).

## The profile (`pizero/carplay-minimal.env`)

| Variable | Profile value | Why |
|---|---|---|
| `RASCAR_CARPLAY_AUDIO` | `off` | No audio stream, and MainAudio is left to the car (the radio keeps playing) |
| `RASCAR_NOWPLAYING` | `off` | No `rascar-app` to read media state from |
| `RASCAR_UDC` | `20980000.usb` | The Pi Zero's OTG controller (default `fc000000.usb`, the Orange Pi) |
| `RASCAR_VIDEO_MODE` | `static` | `none`: no video at all. `static`: encode once, replay. `live`: ffmpeg throughout |
| `RASCAR_VIDEO_SOURCE` | `color:1d1d1f` | Anything but the dashboard: `test`, `color:RRGGBB`, `image:PATH`, `fb[:/dev/fbN]`, `ffmpeg:ARGS` |
| `RASCAR_VIDEO_ENCODER` | `v4l2m2m` | The Pi's VideoCore hardware encoder |
| `RASCAR_VIDEO_FPS` / `_BITRATE_KBPS` | `15` / `2000` | Enough for a still screen |
| `RASCAR_EVENTS_ADDR` | `127.0.0.1:7705` | Car touches/keys as UDP text lines |

### Video modes

- **`none`**: video is a no-op. No encoder, no screen stream: the first `modesChanged` gives
  MainScreen to the car, and a `requestUI` (the CarPlay icon) is answered by restating that. The
  lightest mode. It has not been tested whether a real head unit keeps a session with no screen
  stream; if yours drops it ~7-8 s in, use `static`.
- **`static`** (the default for still sources): one GOP (1 IDR + P-frames, a few KB) is encoded once,
  cached in `/var/lib/rascar/video-cache/` per display size and settings, and replayed from memory.
  The clip for the last display size the car reported is pre-encoded at start, so the first keyframe
  goes out instantly. No encoder runs during a session.
- **`live`**: ffmpeg runs for the whole session (always the case for `fb` and `ffmpeg:` sources).

### Touch and keys

With `RASCAR_EVENTS_ADDR` set, each event is one UDP datagram, in pixels of the encoded picture:

```text
touch down 412 233
touch move 415 240
touch up 415 240
key next
```

Keys: `play`, `pause`, `play-pause`, `next`, `previous`, `stop`, `hook`, `drop`. Try `nc -ulk 7705`.
An app drawing on `/dev/fb0` (with `RASCAR_VIDEO_SOURCE=fb`) can listen there.
`printf 'screen car' > /dev/udp/127.0.0.1/7704` hands the screen back to the car's own UI.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `gadget setup failed`, `no UDC available` | dwc2 not in OTG mode | `dtoverlay=dwc2-role-switch` in `config.txt`, no other `dtoverlay=dwc2…`, no `modules-load=…g_ether` in `cmdline.txt` |
| Nothing in the log when plugged in | Cable in `PWR IN`, or a charge-only cable | Use the port marked **USB** and a data cable |
| `ROLE-SWITCH RESULT: method=none` | No USB role switch in the kernel | `RASCAR_ROLE_SWITCH_CMD`, see above |
| `waiting for car USB device` times out after the switch | dwc2 did not become host | `cat /sys/class/usb_role/*/role` during the attempt; try the rebind fallback |
| `ffmpeg exited … after 0 frame(s)` | Encoder or source | `ffmpeg -encoders \| grep h264_v4l2m2m`, `ls /dev/video11` |
| Session dies right after `screen SETUP` | First encode for a new display size was too slow | Retry: the clip is cached by then |

Everything else (role switch never sent, LPM, NCM) is in [troubleshooting.md](troubleshooting.md#carplay-connection).
