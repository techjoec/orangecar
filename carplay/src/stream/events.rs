//! Car input → whatever draws the picture.
//!
//! Touches and steering-wheel keys from the car are sent, one per UDP datagram,
//! as plain text lines to `RASCAR_EVENTS_ADDR` (default `127.0.0.1:7705`):
//!
//! ```text
//! touch down 412 233      (pixels in the encoded picture)
//! touch move 415 240
//! touch up 415 240
//! key play-pause          (also play, pause, next, previous, stop, hook, drop)
//! ```
//!
//! Nobody listening is fine: the datagrams are simply dropped. Try it with
//! `nc -ulk 7705` on the Pi.

use std::net::UdpSocket;
use std::sync::OnceLock;

fn target() -> &'static Option<(UdpSocket, String)> {
    static T: OnceLock<Option<(UdpSocket, String)>> = OnceLock::new();
    T.get_or_init(|| {
        let addr = std::env::var("RASCAR_EVENTS_ADDR").unwrap_or_else(|_| "127.0.0.1:7705".into());
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
