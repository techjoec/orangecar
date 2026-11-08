//! Video path: produce a picture, H.264-encode it with ffmpeg, and split the
//! output into NAL units for the CarPlay screen packetizer
//! (`airplay::carplay_session`).
//!
//! # Picture source (`RASCAR_VIDEO_SOURCE`)
//!
//! | value              | ffmpeg input                                             |
//! |--------------------|----------------------------------------------------------|
//! | `test` (default)   | `lavfi testsrc2`: a moving test card, proves the stream lives |
//! | `color:RRGGBB`     | `lavfi color`: a solid colour, the cheapest source       |
//! | `image:PATH`       | a still image (PNG/JPEG), looped                         |
//! | `fb` / `fb:/dev/fbN` | the Linux framebuffer (draw anything on the Pi's console/fb) |
//! | `ffmpeg:ARGS`      | raw ffmpeg input arguments, split on whitespace          |
//!
//! Every source is scaled to the car's negotiated display size.
//!
//! # Encoder (`RASCAR_VIDEO_ENCODER`)
//!
//! `v4l2m2m` (the Raspberry Pi's VideoCore hardware encoder, `h264_v4l2m2m`) is
//! the default whenever ffmpeg has it: a Pi Zero W cannot software-encode at a
//! usable frame rate. `x264` and `openh264` are the software fallbacks (useful
//! on a PC bench). The stream is H.264 baseline, no B-frames, one IDR every
//! `RASCAR_VIDEO_KEYINT` frames.

use anyhow::{Context, Result};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct VideoSettings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub keyframe_interval: u32,
}

/// Read a positive integer tuning knob from the environment, falling back to
/// `default` when unset or unparseable.
fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            width: 800,
            height: 480,
            // 15 fps: plenty for a mostly static screen, and leaves the Pi Zero's
            // single core room for scaling + the session.
            fps: env_u32("RASCAR_VIDEO_FPS", 15),
            bitrate_kbps: env_u32("RASCAR_VIDEO_BITRATE_KBPS", 2000),
            // One IDR every second at 15 fps: bounds how long a car forceKeyFrame
            // (answered on the next IDR) or a decoder hiccup can last.
            keyframe_interval: env_u32("RASCAR_VIDEO_KEYINT", 15),
        }
    }
}

/// Where the picture comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Test,
    Color(String),
    Image(String),
    Framebuffer(String),
    Raw(Vec<String>),
}

impl Source {
    pub fn from_env() -> Self {
        Self::parse(std::env::var("RASCAR_VIDEO_SOURCE").ok().as_deref())
    }

    fn parse(raw: Option<&str>) -> Self {
        let raw = raw.map(str::trim).unwrap_or("");
        if raw.is_empty() || raw == "test" {
            return Source::Test;
        }
        if raw == "fb" {
            return Source::Framebuffer("/dev/fb0".into());
        }
        match raw.split_once(':') {
            Some(("color", c)) => Source::Color(c.trim_start_matches('#').to_string()),
            Some(("image", p)) => Source::Image(p.to_string()),
            Some(("fb", p)) => Source::Framebuffer(p.to_string()),
            Some(("ffmpeg", a)) => Source::Raw(a.split_whitespace().map(String::from).collect()),
            _ => {
                warn!("video: RASCAR_VIDEO_SOURCE={raw:?} not understood — using the test card");
                Source::Test
            }
        }
    }

    /// ffmpeg input arguments, plus whether the input still needs scaling.
    fn input_args(&self, w: u32, h: u32, fps: u32) -> (Vec<String>, bool) {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        match self {
            // `-re`: synthetic sources are paced to real time, otherwise ffmpeg
            // generates frames as fast as the CPU allows.
            Source::Test => (v(&["-re", "-f", "lavfi", "-i", &format!("testsrc2=size={w}x{h}:rate={fps}")]), false),
            Source::Color(c) => (v(&["-re", "-f", "lavfi", "-i", &format!("color=c=0x{c}:size={w}x{h}:rate={fps}")]), false),
            Source::Image(p) => (v(&["-re", "-loop", "1", "-framerate", &fps.to_string(), "-i", p]), true),
            Source::Framebuffer(p) => (v(&["-f", "fbdev", "-framerate", &fps.to_string(), "-i", p]), true),
            Source::Raw(args) => (args.clone(), true),
        }
    }
}

/// Which H.264 encoder to drive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum H264Encoder {
    /// Raspberry Pi VideoCore hardware encoder through V4L2 mem2mem.
    V4l2m2m,
    X264,
    OpenH264,
}

/// `RASCAR_VIDEO_ENCODER`, else the best one this ffmpeg has (probed once).
pub fn h264_encoder() -> H264Encoder {
    use std::sync::OnceLock;
    static CHOICE: OnceLock<H264Encoder> = OnceLock::new();
    *CHOICE.get_or_init(|| {
        match std::env::var("RASCAR_VIDEO_ENCODER").ok().as_deref().map(str::trim) {
            Some("v4l2m2m") => return H264Encoder::V4l2m2m,
            Some("x264") => return H264Encoder::X264,
            Some("openh264") => return H264Encoder::OpenH264,
            Some(other) if !other.is_empty() => warn!("video: RASCAR_VIDEO_ENCODER={other:?} unknown — probing"),
            _ => {}
        }
        let text = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-encoders"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let has_m2m_device = std::path::Path::new("/dev/video11").exists();
        if text.contains("h264_v4l2m2m") && has_m2m_device {
            H264Encoder::V4l2m2m
        } else if text.contains("libx264") {
            H264Encoder::X264
        } else if text.contains("libopenh264") {
            H264Encoder::OpenH264
        } else {
            warn!("video: no usable H.264 encoder found in ffmpeg — trying h264_v4l2m2m anyway");
            H264Encoder::V4l2m2m
        }
    })
}

pub struct VideoEncoder {
    settings: VideoSettings,
    source: Source,
    process: Option<Child>,
}

impl VideoEncoder {
    pub fn new(settings: VideoSettings) -> Self {
        Self { settings, source: Source::from_env(), process: None }
    }

    fn build_args(&self, encoder: H264Encoder) -> Vec<String> {
        let s = &self.settings;
        let mut args: Vec<String> = vec!["-hide_banner".into(), "-loglevel".into(), "warning".into()];
        let (input, needs_scale) = self.source.input_args(s.width, s.height, s.fps);
        args.extend(input);
        let mut vf = Vec::new();
        if needs_scale {
            vf.push(format!("fps={},scale={}:{}", s.fps, s.width, s.height));
        }
        vf.push("format=yuv420p".to_string());
        args.extend(["-vf".into(), vf.join(",")]);
        let kbps = format!("{}k", s.bitrate_kbps);
        let gop = s.keyframe_interval.to_string();
        match encoder {
            H264Encoder::V4l2m2m => args.extend([
                "-c:v".into(), "h264_v4l2m2m".into(),
                // 66 = FF_PROFILE_H264_BASELINE (v4l2m2m takes the numeric form).
                "-profile:v".into(), "66".into(),
                "-b:v".into(), kbps,
                "-g".into(), gop,
                "-bf".into(), "0".into(),
                // Re-emit SPS/PPS before every keyframe if the encoder put them
                // in extradata only; the packetizer needs them in-band.
                "-bsf:v".into(), "dump_extra=freq=keyframe".into(),
            ]),
            H264Encoder::X264 => args.extend([
                "-c:v".into(), "libx264".into(),
                "-profile:v".into(), "baseline".into(),
                "-preset".into(), "superfast".into(),
                "-tune".into(), "zerolatency".into(),
                "-b:v".into(), kbps.clone(),
                "-maxrate".into(), kbps.clone(),
                "-bufsize".into(), kbps,
                "-g".into(), gop.clone(),
                "-keyint_min".into(), gop,
                "-sc_threshold".into(), "0".into(),
                "-bf".into(), "0".into(),
                "-x264-params".into(), "repeat-headers=1".into(),
            ]),
            H264Encoder::OpenH264 => args.extend([
                "-c:v".into(), "libopenh264".into(),
                "-profile:v".into(), "constrained_baseline".into(),
                "-b:v".into(), kbps.clone(),
                "-maxrate".into(), kbps,
                "-g".into(), gop,
                "-bf".into(), "0".into(),
            ]),
        }
        args.extend(["-an".into(), "-f".into(), "h264".into(), "pipe:1".into()]);
        args
    }

    pub async fn start(&mut self) -> Result<()> {
        let encoder = h264_encoder();
        let args = self.build_args(encoder);
        info!(
            "video: starting ffmpeg {}x{}@{} source={:?} encoder={encoder:?}",
            self.settings.width, self.settings.height, self.settings.fps, self.source
        );
        let child = Command::new("ffmpeg")
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("spawning ffmpeg — is it installed?")?;
        info!("video: ffmpeg started (pid={})", child.id().unwrap_or(0));
        self.process = Some(child);
        Ok(())
    }

    /// Read a chunk of the elementary stream. `Ok(0)` = EOF (ffmpeg exited).
    pub async fn read_chunk(&mut self, buf: &mut [u8]) -> Result<usize> {
        let child = self.process.as_mut().context("encoder not started")?;
        let stdout = child.stdout.as_mut().context("no stdout")?;
        Ok(stdout.read(buf).await?)
    }

    /// Reap ffmpeg after its stdout hit EOF and report its exit status.
    pub async fn wait_exit(&mut self) -> Result<std::process::ExitStatus> {
        let mut child = self.process.take().context("encoder not started")?;
        child.wait().await.context("waiting for ffmpeg")
    }

    /// Kill the encoder.
    pub async fn stop(&mut self) {
        if let Some(mut child) = self.process.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
            info!("video: ffmpeg stopped");
        }
    }
}

// ---------------------------------------------------------------------------
// NAL parsing (Annex-B) — ported from doubletake mirror.go
// ---------------------------------------------------------------------------

/// Incremental Annex-B NAL splitter. Returns whole NAL units (WITHOUT start
/// code) as they complete; keeps a partial tail buffered across reads.
///
/// `pub(crate)` so the CarPlay screen packetizer (`airplay::carplay_session`)
/// can reuse this proven Annex-B reader instead of duplicating it.
pub(crate) struct NalParser {
    buf: Vec<u8>,
}

impl NalParser {
    pub(crate) fn new() -> Self {
        Self { buf: Vec::with_capacity(512 * 1024) }
    }

    /// Push freshly-read bytes; return the raw NALs (start code stripped) now
    /// fully delimited.
    pub(crate) fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            let start = match find_start_code(&self.buf, 0) {
                Some(s) => s,
                None => {
                    // No start code at all — cap runaway buffering.
                    if self.buf.len() > 1024 * 1024 {
                        let keep = self.buf.len() - 128 * 1024;
                        self.buf.drain(..keep);
                    }
                    break;
                }
            };
            let next = match find_start_code(&self.buf, start + 3) {
                Some(n) => n,
                None => {
                    // Trailing partial NAL: keep from `start` onward.
                    if start > 0 {
                        self.buf.drain(..start);
                    }
                    break;
                }
            };
            let nal = strip_start_code(&self.buf[start..next]).to_vec();
            if !nal.is_empty() {
                out.push(nal);
            }
            self.buf.drain(..next);
        }
        out
    }
}

fn find_start_code(b: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 3 < b.len() {
        if b[i] == 0 && b[i + 1] == 0 {
            if b[i + 2] == 1 {
                return Some(i);
            }
            if i + 3 < b.len() && b[i + 2] == 0 && b[i + 3] == 1 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Strip a leading `00 00 01` or `00 00 00 01` start code.
fn strip_start_code(nal: &[u8]) -> &[u8] {
    if nal.len() > 4 && nal[0] == 0 && nal[1] == 0 && nal[2] == 0 && nal[3] == 1 {
        &nal[4..]
    } else if nal.len() > 3 && nal[0] == 0 && nal[1] == 0 && nal[2] == 1 {
        &nal[3..]
    } else {
        nal
    }
}

/// H.264 NAL unit type (raw NAL, start code already stripped).
pub(crate) fn nal_type(raw: &[u8]) -> u8 {
    raw.first().map(|b| b & 0x1f).unwrap_or(0)
}

/// True if `raw` is the first slice of a new access unit (first_mb_in_slice==0).
pub(crate) fn is_first_slice(raw: &[u8]) -> bool {
    raw.len() >= 2 && (raw[1] & 0x80) != 0
}

/// True if `raw` is the first slice of a new access unit (first_mb_in_slice==0).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_code_and_nal_type() {
        let stream = [0u8, 0, 0, 1, 0x67, 0x42, 0x00, 0x1f];
        let raw = strip_start_code(&stream);
        assert_eq!(raw[0], 0x67);
        assert_eq!(nal_type(raw), 7);
    }

    #[test]
    fn parser_splits_two_nals() {
        let mut p = NalParser::new();
        let data = [
            0, 0, 0, 1, 0x67, 0xAA, // SPS
            0, 0, 0, 1, 0x68, 0xBB, // PPS
            0, 0, 0, 1, 0x65, 0xCC, // IDR (sentinel to close PPS)
        ];
        let nals = p.push(&data);
        assert_eq!(nals.len(), 2);
        assert_eq!(nal_type(&nals[0]), 7);
        assert_eq!(nal_type(&nals[1]), 8);
    }

    #[test]
    fn sources_parse() {
        assert_eq!(Source::parse(None), Source::Test);
        assert_eq!(Source::parse(Some("fb")), Source::Framebuffer("/dev/fb0".into()));
        assert_eq!(Source::parse(Some("fb:/dev/fb1")), Source::Framebuffer("/dev/fb1".into()));
        assert_eq!(Source::parse(Some("image:/a.png")), Source::Image("/a.png".into()));
        assert_eq!(Source::parse(Some("color:#102030")), Source::Color("102030".into()));
        assert_eq!(
            Source::parse(Some("ffmpeg:-f v4l2 -i /dev/video0")),
            Source::Raw(vec!["-f".into(), "v4l2".into(), "-i".into(), "/dev/video0".into()])
        );
    }

    #[test]
    fn hardware_encoder_args_are_baseline_without_b_frames() {
        let enc = VideoEncoder { settings: VideoSettings::default(), source: Source::Test, process: None };
        let args = enc.build_args(H264Encoder::V4l2m2m).join(" ");
        assert!(args.contains("-c:v h264_v4l2m2m"));
        assert!(args.contains("-bf 0"));
        assert!(args.contains("testsrc2=size=800x480"));
        assert!(args.ends_with("-f h264 pipe:1"));
    }

    #[test]
    fn scaled_sources_are_scaled_to_the_car() {
        let mut enc = VideoEncoder { settings: VideoSettings::default(), source: Source::Image("/x.png".into()), process: None };
        enc.settings.width = 1200;
        enc.settings.height = 512;
        let args = enc.build_args(H264Encoder::X264).join(" ");
        assert!(args.contains("scale=1200:512"));
    }
}
