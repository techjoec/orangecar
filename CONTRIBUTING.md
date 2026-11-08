# Contributing

Thanks for helping. The most useful contributions right now are:

1. **Reports from cars.** Did the Pi enumerate? Did the car send the role switch? How far did the
   session get? Fill in the vehicle template in [docs/tested-vehicles.md](docs/tested-vehicles.md) and
   attach the `journalctl -u rascar-carplay` output **after removing anything personal** (the car's VIN
   can show up in iAP2 identification).
2. **Pi Zero hardware results.** The dwc2 role switch and the hardware H.264 encoder are untested in a
   car. Logs from a first attempt are very welcome.
3. **Protocol knowledge.** Corrections or additions to [docs/protocol.md](docs/protocol.md), with the capture
   that proves them.

## Ground rules

- A capture from a real car beats any reference implementation. Say which car and head unit it came from.
- Mark anything you have not verified as unverified.
- Code comments, commit messages and docs are in English.
- Keep `cargo test --workspace` green, run `tools/bench/session_survival_test.sh` after any change in `carplay/`, and `bash -n` on shell scripts.
- Do not commit certificates, keys, captures containing VINs, or anything tied to a person.

## Building

See [README.md](README.md). Cross-compiling for the Pi Zero W (ARMv6) from a PC:

```bash
rustup target add arm-unknown-linux-gnueabihf
pip install ziglang && cargo install cargo-zigbuild
cargo zigbuild --release --target arm-unknown-linux-gnueabihf.2.31 -p rascar-carplay
```

## License

By contributing you agree that your contribution is licensed under the GPL-3.0, like the rest of the project.
