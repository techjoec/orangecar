# Third-party code and credits

This project is licensed under the **GNU GPL v3** (see [LICENSE](LICENSE)). Parts of it are derived from,
or were checked against, other open-source projects. Thank you to their authors: without them this
project would not exist.

## Code derived from other projects

| Project | License | Used for | Where |
|---|---|---|---|
| [usenocturne/iap2-rs](https://github.com/usenocturne/iap2-rs) | GPL-3.0 | iAP2 link layer, control session and auth logic, adapted to the *device* (phone) side | `carplay/src/iap2/` |
| [omarroth/doubletake](https://github.com/omarroth/doubletake) | LGPL-3.0 | AirPlay sender building blocks: RTSP client, binary plist, HomeKit pairing | `carplay/src/airplay/{rtsp,pairing}.rs`, `carplay/src/stream/crypto.rs` |
| Linux kernel `drivers/net/usb/cdc_ncm.c` | GPL-2.0 | Patched CDC-NCM host driver (the patch only, applied to your kernel's own source) | `kernel/cdc_ncm/` |

Because `carplay/` includes GPL-3.0 code, the whole repository is distributed under GPL-3.0.

## Projects used as references (no code copied)

| Project | What we checked against it |
|---|---|
| [catplay-labs/catplay](https://github.com/catplay-labs/catplay) | Wired CarPlay session order, screen frame format, stream descriptions, modes/resources, the `_carplay-ctrl` mDNS packet layout. catplay publishes no license, so none of its code is included; only wire-format facts were checked against it. |
| [lvalen91/carplayd](https://github.com/lvalen91/carplayd) (`iap2-core/src/spec.rs`) | iAP2 message and parameter identifiers |
| [shilapi/xcertplay](https://github.com/shilapi/xcertplay) | iAP2 message directions |
| Carlinkit wireless CarPlay dongle firmware | How a commercial "phone side" brings up the CDC-NCM link |

## Trademarks

CarPlay, iPhone, AirPlay and iAP are trademarks of Apple Inc.
Jaguar and InControl are trademarks of Jaguar Land Rover. This project is not affiliated with,
endorsed by or certified by any of them. It is an independent interoperability project.
