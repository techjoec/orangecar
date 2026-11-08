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
//! # Mode (`RASCAR_VIDEO_MODE`)
//!
//! `static` (default except for `fb` / `ffmpeg:`): one GOP of the picture is
//! encoded once, cached in `RASCAR_VIDEO_CACHE`, and replayed from memory in a
//! loop, so no encoder runs during a session. `live`: ffmpeg runs throughout.
//!
//! # Encoder (`RASCAR_VIDEO_ENCODER`)
//!
//! `v4l2m2m` (the Raspberry Pi's VideoCore hardware encoder, `h264_v4l2m2m`) is
//! the default whenever ffmpeg has it: a Pi Zero W cannot software-encode at a
//! usable frame rate. `x264` and `openh264` are the software fallbacks (useful
//! on a PC bench). The stream is H.264 baseline, no B-frames, one IDR every
//! `RASCAR_VIDEO_KEYINT` frames.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
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
    /// `realtime`: pace synthetic sources to the wall clock (live streaming);
    /// off when pre-encoding a clip, which should finish as fast as possible.
    fn input_args(&self, w: u32, h: u32, fps: u32, realtime: bool) -> (Vec<String>, bool) {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let (mut args, scale) = match self {
            // `-re`: synthetic sources are paced to real time, otherwise ffmpeg
            // generates frames as fast as the CPU allows.
            Source::Test => (v(&["-f", "lavfi", "-i", &format!("testsrc2=size={w}x{h}:rate={fps}")]), false),
            Source::Color(c) => (v(&["-f", "lavfi", "-i", &format!("color=c=0x{c}:size={w}x{h}:rate={fps}")]), false),
            Source::Image(p) => (v(&["-loop", "1", "-framerate", &fps.to_string(), "-i", p]), true),
            Source::Framebuffer(p) => return (v(&["-f", "fbdev", "-framerate", &fps.to_string(), "-i", p]), true),
            Source::Raw(args) => return (args.clone(), true),
        };
        if realtime {
            args.insert(0, "-re".into());
        }
        (args, scale)
    }

    /// Live sources change over time and must be encoded continuously; the
    /// others can be encoded once and replayed (`Mode::Static`).
    fn is_live(&self) -> bool {
        matches!(self, Source::Framebuffer(_) | Source::Raw(_))
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

/// How the stream is produced (`RASCAR_VIDEO_MODE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// ffmpeg runs for the whole session (needed for `fb` / `ffmpeg:` sources).
    Live,
    /// A one-GOP clip (1 IDR + P-frames) is encoded ONCE, cached on disk, and
    /// replayed from memory in a loop: no ffmpeg and almost no CPU during a
    /// session. The default for the test card, colour and image sources.
    Static,
}

impl Mode {
    fn for_source(source: &Source) -> Self {
        match std::env::var("RASCAR_VIDEO_MODE").ok().as_deref().map(str::trim) {
            Some("live") => Mode::Live,
            Some("static") if !source.is_live() => Mode::Static,
            Some("static") => {
                warn!("video: RASCAR_VIDEO_MODE=static ignored for the live source {source:?}");
                Mode::Live
            }
            _ if source.is_live() => Mode::Live,
            _ => Mode::Static,
        }
    }
}

pub struct VideoEncoder {
    settings: VideoSettings,
    source: Source,
    process: Option<Child>,
    replay: Option<Replay>,
}

impl VideoEncoder {
    pub fn new(settings: VideoSettings) -> Self {
        Self { settings, source: Source::from_env(), process: None, replay: None }
    }

    /// ffmpeg arguments. `out`: `None` streams to stdout in real time (live);
    /// `Some((path, frames))` writes a clip of `frames` frames as fast as possible.
    fn build_args(&self, encoder: H264Encoder, out: Option<(&str, u32)>) -> Vec<String> {
        let s = &self.settings;
        let mut args: Vec<String> = vec!["-hide_banner".into(), "-loglevel".into(), "warning".into()];
        let (input, needs_scale) = self.source.input_args(s.width, s.height, s.fps, out.is_none());
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
        match out {
            None => args.extend(["-an".into(), "-f".into(), "h264".into(), "pipe:1".into()]),
            Some((path, frames)) => args.extend([
                "-frames:v".into(), frames.to_string(),
                "-an".into(), "-f".into(), "h264".into(), "-y".into(), path.into(),
            ]),
        }
        args
    }

    pub async fn start(&mut self) -> Result<()> {
        remember_display(self.settings.width, self.settings.height);
        if Mode::for_source(&self.source) == Mode::Static {
            match static_clip(self).await {
                Ok(aus) => {
                    info!(
                        "video: replaying a cached {}-frame clip {}x{}@{} (source={:?}) — no encoder running",
                        aus.len(), self.settings.width, self.settings.height, self.settings.fps, self.source
                    );
                    self.replay = Some(Replay::new(aus, self.settings.fps));
                    return Ok(());
                }
                Err(e) => warn!("video: static clip unavailable ({e:#}) — encoding live instead"),
            }
        }
        let encoder = h264_encoder();
        let args = self.build_args(encoder, None);
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
    /// Cancellation-safe (used inside `select!`).
    pub async fn read_chunk(&mut self, buf: &mut [u8]) -> Result<usize> {
        if let Some(r) = self.replay.as_mut() {
            return Ok(r.read(buf).await);
        }
        let child = self.process.as_mut().context("encoder not started")?;
        let stdout = child.stdout.as_mut().context("no stdout")?;
        Ok(stdout.read(buf).await?)
    }

    /// Reap ffmpeg after its stdout hit EOF and report its exit status.
    pub async fn wait_exit(&mut self) -> Result<std::process::ExitStatus> {
        if self.replay.take().is_some() {
            use std::os::unix::process::ExitStatusExt;
            return Ok(std::process::ExitStatus::from_raw(0));
        }
        let mut child = self.process.take().context("encoder not started")?;
        child.wait().await.context("waiting for ffmpeg")
    }

    /// Kill the encoder (or stop the replay).
    pub async fn stop(&mut self) {
        self.replay = None;
        if let Some(mut child) = self.process.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
            info!("video: ffmpeg stopped");
        }
    }
}

// ---------------------------------------------------------------------------
// Static mode: encode once, cache, replay
// ---------------------------------------------------------------------------

/// Loops over pre-encoded access units (Annex-B) at a fixed frame rate.
struct Replay {
    aus: Arc<Vec<Vec<u8>>>,
    idx: usize,
    offset: usize,
    next_due: tokio::time::Instant,
    period: Duration,
}

impl Replay {
    fn new(aus: Arc<Vec<Vec<u8>>>, fps: u32) -> Self {
        Self {
            aus,
            idx: 0,
            offset: 0,
            next_due: tokio::time::Instant::now(),
            period: Duration::from_secs(1) / fps.max(1),
        }
    }

    /// Cancellation-safe: the only await is the pacing sleep, and no state
    /// changes before it completes.
    async fn read(&mut self, buf: &mut [u8]) -> usize {
        if self.offset == 0 {
            tokio::time::sleep_until(self.next_due).await;
        }
        let au = &self.aus[self.idx];
        let n = buf.len().min(au.len() - self.offset);
        buf[..n].copy_from_slice(&au[self.offset..self.offset + n]);
        self.offset += n;
        if self.offset == au.len() {
            self.offset = 0;
            self.idx = (self.idx + 1) % self.aus.len();
            self.next_due += self.period;
            // Never try to catch up after a stall: just resume the cadence.
            let now = tokio::time::Instant::now();
            if self.next_due + self.period < now {
                self.next_due = now;
            }
        }
        n
    }
}

fn cache_dir() -> PathBuf {
    std::env::var("RASCAR_VIDEO_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/rascar/video-cache"))
}

/// Remember the car's display size so the next start can pre-encode for it.
fn remember_display(w: u32, h: u32) {
    let dir = cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("last_display"), format!("{w}x{h}\n"));
}

fn last_display() -> (u32, u32) {
    std::fs::read_to_string(cache_dir().join("last_display"))
        .ok()
        .and_then(|s| {
            let (w, h) = s.trim().split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((800, 480))
}

/// Everything that changes the encoded clip.
fn clip_key(enc: &VideoEncoder, encoder: H264Encoder) -> String {
    use sha2::{Digest, Sha256};
    let s = &enc.settings;
    let mtime = match &enc.source {
        Source::Image(p) => std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs()),
        _ => 0,
    };
    let desc = format!(
        "v1|{}x{}|{}|{}|{}|{encoder:?}|{:?}|{mtime}",
        s.width, s.height, s.fps, s.bitrate_kbps, s.keyframe_interval, enc.source
    );
    hex::encode(&Sha256::digest(desc.as_bytes())[..8])
}

/// The clip for `enc`'s settings: from memory, else the disk cache, else
/// encoded now with ffmpeg (once) and cached.
async fn static_clip(enc: &VideoEncoder) -> Result<Arc<Vec<Vec<u8>>>> {
    // One encode at a time: the pre-warm and a session may ask together.
    static GEN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    static MEM: std::sync::Mutex<Option<(String, Arc<Vec<Vec<u8>>>)>> = std::sync::Mutex::new(None);

    let encoder = h264_encoder();
    let key = clip_key(enc, encoder);
    let _guard = GEN.lock().await;
    if let Some((k, aus)) = MEM.lock().unwrap().as_ref() {
        if *k == key {
            return Ok(aus.clone());
        }
    }
    let dir = cache_dir();
    let path = dir.join(format!("{key}.h264"));
    let data = match tokio::fs::read(&path).await {
        Ok(d) => {
            info!("video: using cached clip {}", path.display());
            d
        }
        Err(_) => {
            std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            let tmp = dir.join(format!("{key}.tmp.h264"));
            let frames = enc.settings.keyframe_interval.max(1);
            let args = enc.build_args(encoder, Some((tmp.to_str().context("cache path")?, frames)));
            let t0 = std::time::Instant::now();
            info!("video: encoding a {frames}-frame clip once ({encoder:?}) → {}", path.display());
            let status = Command::new("ffmpeg")
                .args(&args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit())
                .kill_on_drop(true)
                .status()
                .await
                .context("spawning ffmpeg — is it installed?")?;
            if !status.success() {
                let _ = std::fs::remove_file(&tmp);
                bail!("ffmpeg exited with {status} while encoding the clip");
            }
            std::fs::rename(&tmp, &path)?;
            info!("video: clip encoded in {} ms", t0.elapsed().as_millis());
            tokio::fs::read(&path).await?
        }
    };
    let aus = match split_access_units(&data) {
        Ok(aus) => Arc::new(aus),
        Err(e) => {
            // A broken cache file must not stick: drop it so the next start re-encodes.
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
    };
    *MEM.lock().unwrap() = Some((key, aus.clone()));
    Ok(aus)
}

/// Pre-encode the clip for the last display size the car asked for, in the
/// background, so a session never waits for ffmpeg. No-op for live sources.
pub fn prewarm() {
    let (width, height) = last_display();
    let enc = VideoEncoder::new(VideoSettings { width, height, ..Default::default() });
    if Mode::for_source(&enc.source) != Mode::Static {
        return;
    }
    tokio::spawn(async move {
        if let Err(e) = static_clip(&enc).await {
            warn!("video: pre-encoding the {width}x{height} clip failed: {e:#}");
        }
    });
}

/// Split an Annex-B stream into access units (each Annex-B framed again), and
/// make sure the first one is a decodable keyframe carrying SPS + PPS.
fn split_access_units(data: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut parser = NalParser::new();
    let mut nals = parser.push(data);
    nals.extend(parser.push(&[0, 0, 0, 1])); // flush the last NAL
    let (mut sps, mut pps) = (None, None);
    let mut aus: Vec<Vec<Vec<u8>>> = Vec::new();
    let mut cur: Vec<Vec<u8>> = Vec::new();
    let mut cur_has_vcl = false;
    for nal in nals {
        let t = nal_type(&nal);
        let is_vcl = (1..=5).contains(&t);
        if t == 9 {
            continue; // access unit delimiter
        }
        if cur_has_vcl && (!is_vcl || is_first_slice(&nal)) {
            aus.push(std::mem::take(&mut cur));
            cur_has_vcl = false;
        }
        match t {
            7 => sps = Some(nal.clone()),
            8 => pps = Some(nal.clone()),
            _ => {}
        }
        cur_has_vcl |= is_vcl;
        cur.push(nal);
    }
    if cur_has_vcl {
        aus.push(cur);
    }
    let first = aus.first_mut().context("the clip has no frames")?;
    if !first.iter().any(|n| nal_type(n) == 5) {
        bail!("the clip does not start with an IDR frame");
    }
    // Make the IDR self-contained even if the encoder sent SPS/PPS elsewhere.
    if !first.iter().any(|n| nal_type(n) == 8) {
        first.insert(0, pps.context("the clip has no PPS")?);
    }
    if !first.iter().any(|n| nal_type(n) == 7) {
        first.insert(0, sps.context("the clip has no SPS")?);
    }
    Ok(aus
        .into_iter()
        .map(|au| {
            let mut out = Vec::new();
            for nal in au {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(&nal);
            }
            out
        })
        .collect())
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
        let enc = VideoEncoder { settings: VideoSettings::default(), source: Source::Test, process: None, replay: None };
        let args = enc.build_args(H264Encoder::V4l2m2m, None).join(" ");
        assert!(args.contains("-c:v h264_v4l2m2m"));
        assert!(args.contains("-bf 0"));
        assert!(args.contains("testsrc2=size=800x480"));
        assert!(args.ends_with("-f h264 pipe:1"));
    }

    #[test]
    fn scaled_sources_are_scaled_to_the_car() {
        let mut enc = VideoEncoder { settings: VideoSettings::default(), source: Source::Image("/x.png".into()), process: None, replay: None };
        enc.settings.width = 1200;
        enc.settings.height = 512;
        let args = enc.build_args(H264Encoder::X264, None).join(" ");
        assert!(args.contains("scale=1200:512"));
        assert!(args.starts_with("-hide_banner -loglevel warning -re "), "live: paced to real time");
        let clip = enc.build_args(H264Encoder::X264, Some(("/c.h264", 15))).join(" ");
        assert!(!clip.contains("-re "), "a clip encodes as fast as possible");
        assert!(clip.ends_with("-frames:v 15 -an -f h264 -y /c.h264"));
    }

    #[test]
    fn live_sources_never_use_the_static_clip() {
        if std::env::var("RASCAR_VIDEO_MODE").is_err() {
            assert_eq!(Mode::for_source(&Source::Test), Mode::Static);
            assert_eq!(Mode::for_source(&Source::Framebuffer("/dev/fb0".into())), Mode::Live);
        }
    }

    #[test]
    fn access_units_split_and_the_idr_gets_its_parameter_sets() {
        // SPS, PPS, IDR | P | P, then a second GOP with an IDR but no SPS/PPS.
        let nals: &[&[u8]] = &[&[0x67, 1, 2, 3], &[0x68, 4], &[0x65, 0x88, 1], &[0x41, 0x9a, 2], &[0x41, 0x9a, 3]];
        let mut stream = Vec::new();
        for n in nals {
            stream.extend_from_slice(&[0, 0, 0, 1]);
            stream.extend_from_slice(n);
        }
        let aus = split_access_units(&stream).unwrap();
        assert_eq!(aus.len(), 3);
        assert_eq!(aus[0], [&[0u8, 0, 0, 1, 0x67, 1, 2, 3][..], &[0, 0, 0, 1, 0x68, 4], &[0, 0, 0, 1, 0x65, 0x88, 1]].concat());
        assert_eq!(aus[2], vec![0, 0, 0, 1, 0x41, 0x9a, 3]);
        // No IDR at all: refused.
        assert!(split_access_units(&[0, 0, 0, 1, 0x41, 0x9a, 2]).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn replay_loops_at_the_frame_rate() {
        let mut r = Replay::new(Arc::new(vec![vec![1; 10], vec![2; 3]]), 10);
        let mut buf = [0u8; 4];
        let t0 = tokio::time::Instant::now();
        let mut got = Vec::new();
        for _ in 0..5 {
            let n = r.read(&mut buf).await;
            got.extend_from_slice(&buf[..n]);
        }
        // 10-byte AU in 4+4+2, then the 3-byte AU, then the first AU again.
        assert_eq!(got, [vec![1; 10], vec![2; 3], vec![1; 4]].concat());
        assert_eq!(t0.elapsed(), Duration::from_millis(200));
    }
}
