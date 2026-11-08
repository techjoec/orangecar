//! USB **device → host** role transition for the Raspberry Pi Zero W's dwc2
//! OTG controller.
//!
//! After the car sends the CarPlay role-switch vendor request (`40 51
//! wValue=1`), the "iPhone" must flip its USB controller from peripheral to
//! host, so it can enumerate the car (which becomes a USB device exposing the
//! iAP2 bulk and CDC-NCM interfaces).
//!
//! # Mechanisms, tried in order
//!
//! 1. **`usb_role` class** — `/sys/class/usb_role/<controller>-role-switch/role`,
//!    write `host` / `device`. The dwc2 driver registers this switch when its
//!    device-tree node has `dr_mode = "otg"` **and** `usb-role-switch`; the
//!    `pizero/overlays/dwc2-role-switch.dts` overlay adds both. dwc2 then
//!    forces the session through its A/B-valid overrides, no ID pin needed.
//! 2. **External command** — `RASCAR_ROLE_SWITCH_CMD` is run as
//!    `<cmd> host` / `<cmd> device` (for kernels without the role switch,
//!    e.g. one that unbinds dwc2 and re-probes it with `dr_mode=host`).
//!
//! The function always ends with one grep-able `ROLE-SWITCH RESULT:` line.

use anyhow::{bail, Result};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use crate::debug::read_attr;

/// The OTG controller's platform device name: `20980000.usb` on the BCM2835
/// (Pi Zero / Zero W / 1). Override with `RASCAR_UDC` (e.g. `fe980000.usb` on
/// a Pi 4).
pub fn controller_name() -> String {
    std::env::var("RASCAR_UDC")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "20980000.usb".to_string())
}

/// What accepted the host-mode write, so exactly that can be undone.
#[derive(Debug)]
enum Accepted {
    RoleAttr(PathBuf),
    Command(String),
}

#[derive(Debug, Default)]
pub struct RoleSwitch {
    accepted: Vec<Accepted>,
}

impl RoleSwitch {
    /// Put the port back into device mode on every mechanism that took the host
    /// write. Best-effort and never fails (it runs on error paths).
    pub fn revert_to_device(&self) {
        for a in &self.accepted {
            match a {
                Accepted::RoleAttr(p) => match fs::write(p, "device") {
                    Ok(()) => info!("role-switch: wrote 'device' → {} (reads back: {})", p.display(), read_attr(p)),
                    Err(e) => warn!("role-switch: could NOT revert {} to device mode: {e}", p.display()),
                },
                Accepted::Command(cmd) => {
                    if let Err(e) = run_cmd(cmd, "device") {
                        warn!("role-switch: '{cmd} device' failed: {e:#}");
                    }
                }
            }
        }
        if !self.accepted.is_empty() {
            info!("role-switch: AFTER revert snapshot: {}", role_snapshot());
        }
    }
}

// ── usbcore quirk for the car's USB device ────────────────────────────────────
//
// The tested head unit (8086:9999 "Gadget MFi v0.1") has a 64-byte string
// descriptor #3 and sends no terminating zero-length packet, so a 255-byte
// string read hangs and wedges it: USB_QUIRK_STRING_FETCH_255 (`a`). It also
// advertises USB2 LPM, which was disabled (`k`) for the first session that
// outlived the car's post-auth deadline.

const USBCORE_QUIRKS: &str = "/sys/module/usbcore/parameters/quirks";
const CAR_QUIRK_KEY: &str = "8086:9999:";
const CAR_QUIRK_FLAGS: &str = "ak";
const CAR_QUIRK_ENTRY: &str = "8086:9999:ak";

/// New quirks parameter value with the car's entry merged in, or `None` when
/// nothing changes.
fn quirks_with_car_entry(current: &str) -> Option<String> {
    let current = current.trim();
    if current.contains(CAR_QUIRK_KEY) {
        let merged: Vec<String> = current
            .split(',')
            .map(|entry| match entry.strip_prefix(CAR_QUIRK_KEY) {
                Some(flags) => {
                    let mut f = flags.to_string();
                    for c in CAR_QUIRK_FLAGS.chars() {
                        if !f.contains(c) {
                            f.push(c);
                        }
                    }
                    format!("{CAR_QUIRK_KEY}{f}")
                }
                None => entry.to_string(),
            })
            .collect();
        let merged = merged.join(",");
        if merged == current { None } else { Some(merged) }
    } else if current.is_empty() {
        Some(CAR_QUIRK_ENTRY.to_string())
    } else {
        Some(format!("{current},{CAR_QUIRK_ENTRY}"))
    }
}

fn apply_car_usb_quirk() {
    let before = match fs::read_to_string(USBCORE_QUIRKS) {
        Ok(v) => v.trim().to_string(),
        Err(e) => {
            warn!("usb quirk: cannot read {USBCORE_QUIRKS}: {e} — car string reads may hang");
            return;
        }
    };
    if let Some(new_value) = quirks_with_car_entry(&before) {
        match fs::write(USBCORE_QUIRKS, &new_value) {
            Ok(()) => info!("usb quirk: before=[{before}] after=[{}]", read_attr(USBCORE_QUIRKS)),
            Err(e) => warn!("usb quirk: writing [{new_value}] FAILED: {e}"),
        }
    }
}

fn run_cmd(cmd: &str, arg: &str) -> Result<()> {
    let status = std::process::Command::new(cmd).arg(arg).status()?;
    if !status.success() {
        bail!("'{cmd} {arg}' exited with {status}");
    }
    Ok(())
}

/// Switch the OTG controller to USB **host** mode.
///
/// Blocking (sysfs writes, maybe a script): call from a blocking context.
pub fn switch_to_host() -> Result<RoleSwitch> {
    let ctrl = controller_name();
    info!("role-switch: requesting USB device→host transition for {ctrl}");
    info!("role-switch: BEFORE snapshot: {}", role_snapshot());
    apply_car_usb_quirk();

    let mut switched = RoleSwitch::default();
    let mut methods: Vec<&'static str> = Vec::new();

    // 1) usb_role class switch(es).
    for sw in usb_role_switches() {
        let attr = sw.join("role");
        if !attr.exists() {
            continue;
        }
        match fs::write(&attr, "host") {
            Ok(()) => {
                info!("role-switch[usb_role]: wrote host → {} (reads back: {})", attr.display(), read_attr(&attr));
                switched.accepted.push(Accepted::RoleAttr(attr));
                methods.push("usb_role");
            }
            Err(e) => warn!("role-switch[usb_role]: write {} failed: {e}", attr.display()),
        }
    }

    // 2) External command.
    if switched.accepted.is_empty() {
        if let Some(cmd) = std::env::var("RASCAR_ROLE_SWITCH_CMD").ok().filter(|c| !c.trim().is_empty()) {
            match run_cmd(cmd.trim(), "host") {
                Ok(()) => {
                    info!("role-switch[cmd]: '{cmd} host' OK");
                    switched.accepted.push(Accepted::Command(cmd.trim().to_string()));
                    methods.push("cmd");
                }
                Err(e) => warn!("role-switch[cmd]: {e:#}"),
            }
        }
    }

    info!("role-switch: AFTER snapshot: {}", role_snapshot());
    if switched.accepted.is_empty() {
        warn!("ROLE-SWITCH RESULT: method=none");
        bail!(
            "role-switch: no mechanism accepted the host write. Is the dwc2-role-switch overlay \
             loaded (ls /sys/class/usb_role)? Otherwise set RASCAR_ROLE_SWITCH_CMD."
        );
    }
    info!("ROLE-SWITCH RESULT: method={} snapshot={}", methods.join(","), role_snapshot());
    Ok(switched)
}

/// Log a [`role_snapshot`] at info with a caller-supplied label.
pub fn log_role_snapshot(label: &str) {
    info!("role-switch: snapshot ({label}): {}", role_snapshot());
}

/// One-line, best-effort dump of what decides or reveals the port role.
pub fn role_snapshot() -> String {
    let mut parts: Vec<String> = Vec::new();
    let switches = usb_role_switches();
    if switches.is_empty() {
        parts.push("usb_role=<no switches>".into());
    }
    for sw in switches {
        let name = sw.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        parts.push(format!("usb_role.{name}={}", read_attr(sw.join("role"))));
    }
    let udcs: Vec<String> = fs::read_dir("/sys/class/udc")
        .map(|rd| {
            rd.flatten()
                .map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    format!("{n}:{}", read_attr(e.path().join("state")))
                })
                .collect()
        })
        .unwrap_or_default();
    parts.push(format!("udc=[{}]", udcs.join(",")));
    parts.join(" ")
}

/// `/sys/class/usb_role/*` switch directories, sorted.
fn usb_role_switches() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(Path::new("/sys/class/usb_role"))
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_snapshot_is_best_effort() {
        let s = role_snapshot();
        assert!(s.contains("udc=["));
    }

    #[test]
    fn car_quirk_is_appended_once() {
        assert_eq!(quirks_with_car_entry("").as_deref(), Some(CAR_QUIRK_ENTRY));
        assert_eq!(quirks_with_car_entry("1234:5678:b").as_deref(), Some("1234:5678:b,8086:9999:ak"));
        assert_eq!(quirks_with_car_entry(CAR_QUIRK_ENTRY), None);
    }

    #[test]
    fn older_car_entry_is_upgraded_with_no_lpm() {
        assert_eq!(quirks_with_car_entry("8086:9999:a").as_deref(), Some("8086:9999:ak"));
    }

    #[test]
    fn the_default_controller_is_the_pi_zero_otg_port() {
        if std::env::var("RASCAR_UDC").is_err() {
            assert_eq!(controller_name(), "20980000.usb");
        }
    }
}
