//! Phase 4 — **CarPlay control session** (RTSP over the CDC-NCM link).
//!
//! * `rtsp`: RTSP client + binary plist + HAP frame encryption.
//! * `pairing`: HomeKit pair-setup (SRP-6a, PIN 3939) / pair-verify, persisted identity.
//! * `carplay_session`: SETUP, `/info`, RECORD, `modesChanged`, screen stream, HID.
//! * `hid`: HID report descriptors and key decoding (logged only in this build).
//!
//! # Licensing
//!
//! `rtsp` and `pairing` are ported from `github.com/omarroth/doubletake`
//! (LGPL-3.0), compatible with this crate's GPL-3.0.

#![allow(dead_code)]

pub mod carplay_session;
pub mod hid;
pub mod pairing;
pub mod rtsp;
