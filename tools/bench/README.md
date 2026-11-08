# Bench: test a CarPlay session without a car

`carplay_receiver_sim.py` is a CarPlay **receiver** (head unit) for a PC. It mimics the Jaguar F-PACE
closely enough to run the whole sender path of `rascar-carplay` (RTSP, plists, modes and resources, the
128-byte framed H.264 screen channel, audio streams, time sync, telephony), and it reproduces the car's
fatal behaviours so they show up on a laptop:

- **audio-resource trap**: if the sender gives MainAudio to the car in `modesChanged` and then SETUPs a
  MainAudio stream anyway, the simulator stops answering, as the car does;
- **screen hand-back**: the screen goes to the car's native UI and comes back on `requestUI`;
- **late replies**: the audio SETUP answered 3.2 s late, as seen in the car.

It skips the USB, iAP2 and CDC-NCM layers: `rascar-carplay --carplay-endpoint HOST:PORT` dials it
directly over TCP.

```bash
cargo build --release -p rascar-carplay
tools/bench/session_survival_test.sh            # 2 scenarios, ~1 minute
```

Two scenarios (screen only, and a screen hand-back to the car's UI and back). Each must keep the session
alive past 15 s with at least 100 video frames. Expected output ends with
`RESULT: session survives in every configuration.` Needs `ffmpeg` with `libx264` and IPv6 (the sender
binds its timing socket on `[::]`).
