//! Media plumbing for the CarPlay screen stream.
//!
//!   * [`video`]: ffmpeg capture of a picture source + H.264 encode
//!     (the Pi's hardware encoder by default) and NAL parsing.
//!   * [`crypto`]: stream ciphers (AES-128-CTR / ChaCha20-Poly1305).
//!   * [`events`]: car touches/keys forwarded as text over UDP.
//!   * [`screen`]: who owns the car's main screen (hand-back to the car's UI).

pub mod crypto;
pub mod events;
pub mod screen;
pub mod video;
