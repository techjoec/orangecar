//! Car input → whatever draws the picture, when that is not the dashboard.
//!
//! When `RASCAR_EVENTS_ADDR` is set (e.g. `127.0.0.1:7705`, as in the minimal
//! `pizero/` profile), touches and steering-wheel keys from the car are sent,
//! one per UDP datagram, as plain text lines:
//!
//! ```text
//! touch down 412 233      (pixels in the encoded picture)
//! touch move 415 240
//! touch up 415 240
//! key play-pause          (also play, pause, next, previous, stop, hook, drop)
//! ```
//!
//! Unset or `off`: nothing is sent. Nobody listening is fine too: the
//! datagrams are simply dropped. Try it with `nc -ulk 7705`.

use std::net::UdpSocket;
use std::sync::OnceLock;

fn target() -> &'static Option<(UdpSocket, String)> {
    static T: OnceLock<Option<(UdpSocket, String)>> = OnceLock::new();
    T.get_or_init(|| {
        let addr = std::env::var("RASCAR_EVENTS_ADDR").unwrap_or_default();
        if addr.is_empty() || addr == "off" {
            return None;
        }
        let sock = UdpSocket::bind("127.0.0.1:0").ok()?;
        sock.set_nonblocking(true).ok()?;
        Some((sock, addr))
    })
}

/// Best-effort: never blocks, never fails.
pub fn send(line: &str) {
    if let Some((sock, addr)) = target() {
        let _ = sock.send_to(line.as_bytes(), addr.as_str());
    }
}
