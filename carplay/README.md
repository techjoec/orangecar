# rascar-carplay — the CarPlay "iPhone"

`rascar-carplay` makes a Raspberry Pi Zero W behave like an iPhone on a car's
wired CarPlay USB port. It enumerates as an Apple device, answers the car's
USB role-switch request, becomes USB host, speaks iAP2 to the head unit,
brings up the CDC-NCM link, finds the car's CarPlay receiver and then runs a
CarPlay session that streams an H.264 picture to the car's screen. Touches and
steering-wheel keys come back as text lines on UDP.

No MFi authentication coprocessor is involved: the car is the iAP2
*accessory*, the Pi is the *device*, and the device side is the verifier.

The protocol code was proven on a Jaguar F-PACE (Harman InControl head unit,
legacy CarPlay receiver `srcvers 280.33.12.1`) from an Orange Pi 5 Plus. The
findings behind it are in [`docs/protocol.md`](../docs/protocol.md).

## Modules

| Path | What it does |
|------|--------------|
| `src/main.rs` | Orchestrator: CLI, phase sequencing, retries, re-enumeration handling, bench entry point. |
| `src/usb/` | **Phase 1, USB device side.** `gadget.rs` builds the configfs gadget (Apple VID/PID, four iPhone-shaped configurations, persisted USB serial); `ffs.rs` runs the FunctionFS ep0 loop and answers the vendor requests up to the CarPlay role switch (`40 51 wValue=1`). |
| `src/iap2/` | **Phase 2, iAP2 device stack.** `packet.rs`/`link.rs` = link layer; `control.rs` = control session (identification, accept-all authentication, post-auth notifications, location). |
| `src/host/` | **Phase 3, USB host side.** `role_switch.rs` flips the dwc2 OTG port to host (`/sys/class/usb_role`, or `RASCAR_ROLE_SWITCH_CMD`); `usb_host.rs` finds the car and opens the iAP2 bulk pipe; `ncm.rs` brings up the CDC-NCM interface and discovers the receiver; `ctrl_advertise.rs` + `mdns_ctrl.rs` answer the car's `_carplay-ctrl._tcp` browse. |
| `src/airplay/` | **Phase 4, CarPlay control.** `rtsp.rs` (RTSP + binary plist), `pairing.rs` (pair-setup SRP-6a / pair-verify, persisted identity), `carplay_session.rs` (SETUP, `/info`, RECORD, `modesChanged`, keepalive, screen stream, HID), `hid.rs` (HID descriptors and key decoding). |
| `src/stream/` | **Phase 5, screen.** `video.rs` produces the picture and H.264-encodes it with ffmpeg (once, then replays a cached clip, for still sources); `crypto.rs` encrypts frames; `events.rs` forwards car touches/keys over UDP; `screen.rs` hands the screen back to the car's native UI and takes it again. |
| `src/debug.rs` | Phase timers, wire tracing, the `env_flag` helper used by most knobs. |

## Run phases

1. **Apple enumeration.** Create the `carplay` configfs gadget, mount one
   FunctionFS instance per configuration, bind the UDC and answer ep0 until the
   car sends the CarPlay role-switch request.
2. **Role switch.** Acknowledge it, wait `RASCAR_ROLE_SWITCH_DELAY_MS`, remove
   the gadget and switch the OTG port to host mode.
3. **iAP2 + NCM.** Enumerate the car as a USB device, open its iAP2 bulk
   interface and run the device side of iAP2: link negotiation,
   identification, accept-all authentication, post-auth notifications,
   location subscription. The CDC-NCM interface comes up in
   parallel, and the car re-enumerates once after authentication, which is
   normal.
4. **CarPlay control.** Find the car's receiver (mDNS, or a direct dial on the
   link if no record shows up), pair, run `SETUP`, `GET /info`, `RECORD` and
   the first `modesChanged`.
5. **Stream.** Screen stream (H.264, encrypted), touch and HID back from the
   car, keepalive, screen hand-back on request. No audio: MainAudio stays the
   car's.

Requirements: root (configfs, FunctionFS, usbfs, raw sockets), dwc2 in OTG mode
with `usb-role-switch` (`pizero/overlays/`), the patched `cdc_ncm` module from
`kernel/cdc_ncm/`, `ffmpeg`, `avahi-utils`, `iproute2`. See `pizero/`.

### Command line

```bash
sudo rascar-carplay [--debug]                       # full run against a car
rascar-carplay --carplay-endpoint HOST:PORT         # bench: CarPlay session (phases 4-5) against a receiver
rascar-carplay --mdns-selftest 'fe80::1%usb0'       # bench: run only the _carplay-ctrl responder ~30 s
```

`--debug` raises the log level; `RASCAR_WIRE_TRACE=1` adds hex dumps of USB,
iAP2 and RTSP traffic. Log lines carry grep-able markers such as
`Phase <X> START`, `ROLE-SWITCH RESULT`, `iAP2 LINK STATE`.

## Environment variables

Booleans read through `env_flag` accept `1/on/true/yes` and `0/off/false/no`.
Variables marked *opt-in* only react to `1/on/true/yes`; *opt-out* ones only
to `0/off`. Unless stated otherwise, an unset or unparsable value means the
default.

### USB device side (phase 1)

| Variable | Default | Meaning |
|----------|---------|---------|
| `RASCAR_USB_SERIAL` | unset | Pin the USB serial (exactly 40 hex characters). Unset: use the persisted random serial. |
| `RASCAR_USB_SERIAL_STORE` | `/var/lib/rascar/usb_serial` | File holding the persisted random serial. Delete it to get a new identity. |
| `RASCAR_ROTATE_SERIAL` | off (opt-in) | Generate a new serial after a failed attempt, so the head unit sees a new iPhone. Ignored when the serial is pinned. |
| `RASCAR_UAC1` | on (`off` disables) | Let the kernel's `f_uac1` provide the audio interfaces of configuration 2 with proper class descriptors. |
| `RASCAR_HIDE_CARPLAY` | off (opt-in) | Answer `C0 53` with "no role-switch capability" (present as an iPhone without CarPlay; diagnostics only). |
| `RASCAR_ROLE_SWITCH_GRACE_MS` | `45000` (min 500) | How long to wait for the role-switch request before re-presenting the gadget. |

### USB host side, NCM and mDNS (phase 3)

| Variable | Default | Meaning |
|----------|---------|---------|
| `RASCAR_UDC` | `20980000.usb` | OTG controller platform device (`fe980000.usb` on a Pi 4). |
| `RASCAR_ROLE_SWITCH_CMD` | unset | Command run as `<cmd> host` / `<cmd> device` when no `/sys/class/usb_role` switch exists. |
| `RASCAR_ROLE_SWITCH_DELAY_MS` | `10` (max 2000) | Delay between acknowledging the role switch and switching to host. |
| `RASCAR_EARLY_NCM` | off (opt-in) | Bring the NCM link up during the iAP2 handshake instead of after authentication. |
| `RASCAR_NCM_HOLD` | off (opt-in) | Use the patched module's `hold_apple_gadget` parameter to keep `cdc_ncm` off the car until iAP2 authentication is done. |
| `RASCAR_DEFER_NCM_BIND` | off (opt-in) | Older, known-broken way of deferring the `cdc_ncm` bind (bench only). |
| `RASCAR_POST_AUTH_RESET` | off (opt-in) | Port-reset the car's USB device right after iAP2 authentication. |
| `RASCAR_REENUM_TIMEOUT_MS` | `1500` (max 30000) | How long to wait for the car to re-enumerate after authentication. |
| `RASCAR_ENUM_THEN_IDLE_MS` | unset | Control experiment: enumerate the car, then do nothing for this many ms and exit. |
| `RASCAR_CTRL_VIA_AVAHI` | off | Publish `_carplay-ctrl._tcp` through avahi instead of the built-in raw responder. |
| `RASCAR_CTRL_INSTANCE` | `iPhone` | `_carplay-ctrl` service instance name. |
| `RASCAR_CTRL_HOSTLABEL` | `iPhone` | Host label used in the `_carplay-ctrl` record. |
| `RASCAR_CTRL_SRCVERS` | `566.25.21` | `srcvers` TXT value of the `_carplay-ctrl` record. |
| `RASCAR_CTRL_DEVICE_ID` | built-in device MAC | `id` TXT value, avahi path only (the raw responder always uses the built-in MAC). |

### iAP2 (phase 3)

| Variable | Default | Meaning |
|----------|---------|---------|
| `RASCAR_IAP2_GATE` | on | Never send a message the accessory did not list in `MessagesReceivedFromDevice`. |
| `RASCAR_IAP2_GPGSV` | on | Also request `GPGSV` sentences when the car offers them. |
| `RASCAR_IAP2_KEEP_LOCATION` | on | Keep the location subscription open (off sends `StopLocationInformation` right after starting it). |
| `RASCAR_IAP2_WIRELESS_CARPLAY` | unset (message omitted) | `available` / `unavailable`: send `WirelessCarPlayUpdate` (0x4E0D) with that value. |
| `RASCAR_CAR_GPS_PATH` | `/var/lib/rascar-debug/car-gps.nmea` | Append-only file receiving the car's GPS (NMEA sentences, from iAP2 location). |

### CarPlay session (phase 4)

| Variable | Default | Meaning |
|----------|---------|---------|
| `RASCAR_SKIP_PHASE4` | off | Split experiment: keep iAP2 serviced but never start AirPlay. |
| `RASCAR_AIRPLAY_FALLBACK` | on | Dial the car directly on the NCM link when no AirPlay mDNS record appears. |
| `RASCAR_WAIT_FOR_INVITE_MS` | `0` | Wait up to this long for the car's `/ctrl-int/1/connect` invite before dialling. |
| `RASCAR_HAP_STORE` | `/var/lib/rascar/hap_identity` | Persisted pairing identity and learned accessory key (pair-setup once, pair-verify afterwards). |
| `RASCAR_AUTH_SETUP` | off (only `1` enables) | Perform `/auth-setup` (MFiSAP) for receivers that require it. |
| `RASCAR_ET` | `1` | `et` (encryption type) value sent in SETUP; 1 = none. |
| `RASCAR_INITIAL_SCREEN_SYNC` | on | First restate the car's initial screen ownership before taking the screen, when the car's take constraint allows it. |
| `RASCAR_KEEPALIVE_SECS` | `1` (clamped 1..30) | Interval of the keepalive ping to the receiver's `keepAlivePort`. |

### Streams (phase 5)

| Variable | Default | Meaning |
|----------|---------|---------|
| `RASCAR_VIDEO_SOURCE` | `test` | Picture: `test`, `color:RRGGBB`, `image:PATH`, `fb[:/dev/fbN]`, `ffmpeg:ARGS`. |
| `RASCAR_VIDEO_MODE` | `static` (`live` for `fb`/`ffmpeg:`) | `static`: encode a one-GOP clip once, cache it, replay it (no encoder during a session). `live`: run ffmpeg for the whole session. `none`: no video at all; MainScreen is left to the car's UI and no screen stream is opened. |
| `RASCAR_VIDEO_CACHE` | `/var/lib/rascar/video-cache` | Where static clips (and the last display size, for the pre-encode at start) are kept. |
| `RASCAR_VIDEO_ENCODER` | probed | `v4l2m2m` (Pi hardware), `x264` or `openh264`. |
| `RASCAR_VIDEO_FPS` | `15` | Screen stream frame rate. |
| `RASCAR_VIDEO_BITRATE_KBPS` | `2000` | H.264 bitrate. |
| `RASCAR_VIDEO_KEYINT` | `15` | Keyframe interval in frames. |
| `RASCAR_EVENTS_ADDR` | `127.0.0.1:7705` | Where car touches/keys are sent as UDP text lines (`off` disables). |
| `RASCAR_SCREEN_HANDBACK` | on | Honour requests to hand the screen back to the car's native UI. |

### Debug and build

| Variable | Default | Meaning |
|----------|---------|---------|
| `RASCAR_WIRE_TRACE` | off (only `1` enables) | Hex-dump USB bulk transfers, iAP2 headers/messages and RTSP bodies. |
| `RASCAR_BUILD_ID` | `dev` | **Compile-time** (`option_env!`): build identifier printed at start-up. |

## Building and testing

```bash
cargo check -p rascar-carplay
cargo test  -p rascar-carplay
```

The unit tests run on any Linux host; they use captured byte sequences from the
tested car (identifiers such as the head unit's serial number are replaced by
same-length placeholders). Cross-compile for `arm-unknown-linux-gnueabihf` (ARMv6) to
run on a Pi Zero W.

## Licensing

This crate is **GPL-3.0** (see [`LICENSE`](../LICENSE)).

- The iAP2 stack in `src/iap2/` is derived from
  [usenocturne/iap2-rs](https://github.com/usenocturne/iap2-rs) (GPL-3.0):
  `packet.rs` nearly verbatim, `link.rs` framing verbatim with negotiation
  inverted to the device role, `control.rs` framing kept with identification
  and authentication inverted. Vendoring that code makes the whole crate
  GPL-3.0. Details in the header of `src/iap2/mod.rs`.
- The AirPlay building blocks in `src/airplay/` (`rtsp`, `pairing`) and the
  frame encryption in `src/stream/crypto.rs` are ported from
  [omarroth/doubletake](https://github.com/omarroth/doubletake) (LGPL-3.0),
  which is compatible with GPL-3.0.
- [catplay-labs/catplay](https://github.com/catplay-labs/catplay) was used as
  a reference for the wire format: InitialSetup fields, stream descriptions,
  modes/resources, iAP2 post-auth messages and Now Playing encoding, the
  `_carplay-ctrl` mDNS record, and some test vectors (the 399-byte mDNS
  announcement, an SRP-6a proof vector, a media-button HID descriptor). No
  catplay code is included; the implementation here is written independently.

Apple, CarPlay, AirPlay, iPhone and FairPlay are trademarks of Apple Inc. This
project is not affiliated with or endorsed by Apple.
