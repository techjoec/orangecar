# Pi Zero CarPlay "iPhone"

**A Raspberry Pi Zero W that plugs into a car's wired CarPlay port and plays the iPhone.**

This is a stripped-down version of OrangeCar, an Orange Pi 5 Plus that did the same job with a full
dashboard. All that is left is the part that convinces the car a CarPlay iPhone is connected, and
then streams a picture to the car's screen:

- the Pi enumerates as an Apple iPhone on its USB OTG port;
- it answers the car's CarPlay role-switch request and becomes USB host;
- it runs the phone side of iAP2 (identification, authentication) and brings up the CDC-NCM link;
- it opens a CarPlay session (RTSP, pairing, modes) and streams H.264 video to the car screen;
- touches and steering-wheel keys from the car come back as text lines on UDP.

The dashboard, maps, music, OBD-II, 4G, phone calls, Now Playing, OTA updates and the Android Auto
experiments are gone. Audio is gone too: no capture, no audio stream, and the car's audio is never
claimed, so the radio keeps playing. Video for a still picture is encoded once and replayed (see
[static mode](#whats-on-the-screen)), so a session costs the Pi almost no CPU.

You don't need an iPhone, an MFi chip or a jailbreak. The car authenticates *itself* to the phone,
so the phone side has nothing to prove.

> **Status: untested on a Pi Zero in a car.** The protocol code is the code that ran end to end in a
> Jaguar F-PACE (Harman InControl) on an Orange Pi 5 Plus. Two parts are new and need the hardware
> to verify: the dwc2 role switch on the Pi Zero and the Pi's hardware H.264 encoder. The session
> passes the bench simulator (`tools/bench/`). Please report what happens with your car.

## What you need

- **Raspberry Pi Zero W** (or Zero / Zero 2 W; any Pi whose OTG port is driven by `dwc2`).
- A micro-SD card with **Raspberry Pi OS Lite (Bookworm, 32-bit)**.
- A **micro-USB data cable** from the Pi's **USB** port (not `PWR IN`) to the car's CarPlay USB
  port. That port also powers the Pi, because on the Zero the data port's VBUS feeds the 5 V rail.
  Optional: power `PWR IN` from a separate 5 V supply, so the Pi is booted before the car looks.
- A Linux or macOS PC to cross-compile the binary.

## Build (on a PC)

The Pi Zero W is ARMv6 (`armv6l`). Use the `arm-unknown-linux-gnueabihf` target: the armv7 target
builds a binary that crashes on a Zero.

```bash
rustup target add arm-unknown-linux-gnueabihf
pip install ziglang && cargo install cargo-zigbuild
cargo zigbuild --release --target arm-unknown-linux-gnueabihf.2.31 -p rascar-carplay
# -> target/arm-unknown-linux-gnueabihf/release/rascar-carplay  (~2.5 MB)
```

## Install (on the Pi)

Copy the repository and the binary to the Pi, then:

```bash
sudo pizero/install.sh target/arm-unknown-linux-gnueabihf/release/rascar-carplay
sudo reboot
```

`install.sh`:

1. installs `ffmpeg`, `avahi-daemon`/`avahi-utils`, `iproute2` and the kernel headers;
2. builds `pizero/overlays/dwc2-role-switch.dts` into the boot partition and sets
   `dtoverlay=dwc2-role-switch` in `config.txt`. This puts `dwc2` in OTG mode **with a software USB
   role switch**, which is how the Pi turns from USB device into USB host when the car asks;
3. loads `libcomposite` and `usb_f_fs` at boot and keeps NetworkManager and udev off the car's
   network link;
4. installs `/usr/local/bin/rascar-carplay`, the `rascar-carplay.service` systemd unit and
   `/etc/rascar/carplay.env`;
5. builds the **patched `cdc_ncm` kernel module** (`kernel/cdc_ncm/`). The stock driver cannot bring
   up the car's network link (see `kernel/README.md`). This takes several minutes on a Zero.

After the reboot, check:

```bash
ls /sys/class/usb_role                 # 20980000.usb-role-switch
cat /sys/class/usb_role/*/role         # device
cat /sys/module/cdc_ncm/version        # rascar-3
ffmpeg -hide_banner -encoders | grep h264_v4l2m2m
```

If `/sys/class/usb_role` is empty, your kernel was built without `CONFIG_USB_ROLE_SWITCH`. Set
`RASCAR_ROLE_SWITCH_CMD=/usr/local/sbin/rascar-role-switch-rebind` in `/etc/rascar/carplay.env`.
That fallback unbinds `dwc2`, applies a `dr_mode=host` runtime overlay and binds it again. It is
slower and less tested.

Plug the Pi into the car and watch:

```bash
journalctl -fu rascar-carplay
```

The milestones, in order: `FFS event ENABLE`, `CARPLAY ROLE-SWITCH REQUEST`, `ROLE-SWITCH RESULT`,
`SessionReady`, `← RECORD OK`, `→ VideoConfig`, `→ keyframe`, `frames streamed to car`.

## What's on the screen

Set `RASCAR_VIDEO_SOURCE` in `/etc/rascar/carplay.env`:

| Value | Picture |
|---|---|
| `test` (default) | a moving test card, so you can see the stream is live |
| `color:1d1d1f` | a solid colour (the cheapest source) |
| `image:/etc/rascar/screen.png` | a still image, scaled to the car's screen |
| `fb` or `fb:/dev/fb0` | the Pi's framebuffer: draw anything there (a console, an SDL/fbdev app) |
| `ffmpeg:<args>` | raw ffmpeg input arguments, e.g. `ffmpeg:-f v4l2 -i /dev/video0` for a camera |

Every source is scaled to the display size the car reports, and encoded with the Pi's hardware H.264
encoder (`h264_v4l2m2m`).

**Static mode (default for `test`, `color:` and `image:`).** The picture is encoded **once**, as a
one-second clip (one keyframe plus near-empty P-frames, a few KB), cached in
`/var/lib/rascar/video-cache/` and then replayed from memory in a loop. During a session no encoder
runs at all: the Pi only encrypts and sends a few KB per second. The clip for the last display size
the car reported is pre-encoded at service start, so the first frame goes out immediately. A new car
(new size), image, or setting costs one encode (a second or two) at the first session. `fb` and
`ffmpeg:` are live sources and always run ffmpeg; `RASCAR_VIDEO_MODE=live` forces live encoding for
the others too. Other knobs: `RASCAR_VIDEO_FPS` (15), `RASCAR_VIDEO_BITRATE_KBPS` (2000),
`RASCAR_VIDEO_KEYINT` (15), `RASCAR_VIDEO_ENCODER` (`v4l2m2m` / `x264` / `openh264`).

## Touch and buttons

Car input is sent as one UDP datagram per event to `RASCAR_EVENTS_ADDR` (default `127.0.0.1:7705`),
in pixels of the encoded picture:

```text
touch down 412 233
touch move 415 240
touch up 415 240
key next
```

Key names: `play`, `pause`, `play-pause`, `next`, `previous`, `stop`, `hook`, `drop`. Try
`nc -ulk 7705` on the Pi. Whatever draws the picture (for example an app on `/dev/fb0` with
`RASCAR_VIDEO_SOURCE=fb`) can listen there.

`printf 'screen car' > /dev/udp/127.0.0.1/7704` hands the car screen back to the car's own UI.
Pressing the CarPlay icon on the car (or `screen phone`) takes it back.

## Limits on a Pi Zero W

- **Timing.** The tested car tears the session down if the first keyframe is late (about 7.8 s from
  USB enumeration). In static mode the cached clip is sent instantly. The exceptions are the first
  session with a new display size and live sources (`fb`, `ffmpeg:`), which start ffmpeg at screen
  SETUP; that is slow on a Zero. If such a session dies right after `screen SETUP`, just retry: the
  clip is cached by then.
- **Boot time.** A Zero takes ~30 s to boot. If the car powers it, the car may give up before the Pi
  is ready. Unplug and replug, or power the Pi separately.
- **USB endpoints.** The Pi's dwc2 has 7 device endpoints. The largest iPhone configuration needs 7.
  It should fit; if the car never selects a configuration, report it.
- Wired CarPlay only. No audio, no Siri, no Now Playing.

## Bench test (no car, no Pi)

```bash
cargo build --release -p rascar-carplay
tools/bench/session_survival_test.sh
```

This runs the real CarPlay session against a simulated head unit over TCP (USB, iAP2 and NCM are
skipped) and checks that it stays alive with video flowing, including a screen hand-back. It needs
`ffmpeg` with `libx264` and IPv6 on the PC.

## Repository layout

```
carplay/        rascar-carplay: USB gadget, role switch, iAP2, NCM/mDNS, CarPlay session, H.264 stream
pizero/         Pi Zero W setup: dwc2 overlays, install script, systemd unit, env example
kernel/cdc_ncm/ patch + build script for the CDC-NCM driver the car link needs
tools/bench/    CarPlay receiver simulator and the session survival test
docs/           protocol notes from the original Orange Pi build (still valid for the protocol)
```

[docs/protocol.md](docs/protocol.md) explains wired CarPlay from the phone side, step by step and
byte by byte. [carplay/README.md](carplay/README.md) lists every setting.
[docs/troubleshooting.md](docs/troubleshooting.md) maps symptoms to causes.

## Legal

This is an independent interoperability project, not affiliated with or endorsed by Apple or any car
maker. CarPlay, iPhone and AirPlay are trademarks of Apple Inc. Use it on your own car, at your own
risk, and keep your eyes on the road.

Licensed under the **GPL-3.0**. See [LICENSE](LICENSE) and [THIRD_PARTY.md](THIRD_PARTY.md).
