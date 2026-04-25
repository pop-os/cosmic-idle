//! Dim-idle: fade display backlight down while idle, restore on wake.
//!
//! Writes brightness via logind's Session.SetBrightness (doesn't trigger the
//! COSMIC brightness OSD that pops up when CosmicSettingsDaemon's property
//! is changed). Reads current/max from sysfs.

use std::fs;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

const SYSFS_BACKLIGHT_DIR: &str = "/sys/class/backlight";

const FADE_STEPS: u32 = 30;

pub struct DimState {
    original_brightness: Option<u32>,
    cancel: Arc<AtomicBool>,
}

impl DimState {
    pub fn new() -> Self {
        Self {
            original_brightness: None,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_dimmed(&self) -> bool {
        self.original_brightness.is_some()
    }

    pub fn start_dim(&mut self, target_percent: u8, fade_ms: u32) {
        if self.original_brightness.is_some() {
            return;
        }
        let Some((device, current, max)) = read_backlight() else {
            log::warn!("dim: no backlight found");
            return;
        };
        if current == 0 {
            return;
        }
        let target_percent = target_percent.min(100);
        // 0% means fully off; otherwise compute and floor at 0 (caller decides)
        let target =
            ((current as f32) * (target_percent as f32) / 100.0).round() as u32;
        if target >= current {
            return;
        }
        log::info!(
            "dim: {} {} -> {} (max {}) over {}ms",
            device, current, target, max, fade_ms
        );
        self.original_brightness = Some(current);

        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let device_clone = device.clone();

        std::thread::spawn(move || {
            fade_thread(device_clone, current, target, fade_ms.max(100), cancel);
        });
    }

    pub fn restore(&mut self, fade_ms: u32) {
        let Some(original) = self.original_brightness.take() else {
            return;
        };
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();

        let Some((device, current, _)) = read_backlight() else {
            return;
        };
        if current >= original {
            return;
        }
        log::info!("dim: restore {} -> {}", current, original);
        std::thread::spawn(move || {
            // Restore in ~150ms regardless of fade_ms for responsiveness
            fade_thread(device, current, original, 150.max(fade_ms / 4).min(fade_ms), cancel);
        });
    }

    /// Synchronously snap brightness back to the original and clear state.
    /// Called right before the screen is turned off, so the hardware holds
    /// the correct value when it powers back on.
    pub fn snap_restore(&mut self) {
        let Some(original) = self.original_brightness.take() else {
            return;
        };
        self.cancel.store(true, Ordering::Relaxed);
        let Some((device, _, _)) = read_backlight() else {
            return;
        };
        log::info!("dim: snap_restore to {}", original);
        let _ = set_brightness_via_logind(&device, original);
    }
}

fn fade_thread(device: String, from: u32, to: u32, fade_ms: u32, cancel: Arc<AtomicBool>) {
    let step_ms = (fade_ms / FADE_STEPS).max(8);
    for step in 1..=FADE_STEPS {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let progress = step as f32 / FADE_STEPS as f32;
        let eased = 1.0 - (1.0 - progress).powi(2);
        let value = (from as f32 + (to as f32 - from as f32) * eased).round() as u32;
        if let Err(e) = set_brightness_via_logind(&device, value) {
            log::debug!("dim: SetBrightness failed: {e:?}");
            return;
        }
        std::thread::sleep(Duration::from_millis(step_ms as u64));
    }
}

/// Returns (device_name, current_brightness, max_brightness) for the first
/// backlight device found.
fn read_backlight() -> Option<(String, u32, u32)> {
    let entries = fs::read_dir(SYSFS_BACKLIGHT_DIR).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let base = entry.path();
        let cur = fs::read_to_string(base.join("brightness"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
        let max = fs::read_to_string(base.join("max_brightness"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
        if let (Some(cur), Some(max)) = (cur, max) {
            return Some((name, cur, max));
        }
    }
    None
}

fn set_brightness_via_logind(device: &str, value: u32) -> zbus::Result<()> {
    let conn = zbus::blocking::Connection::system()?;
    let proxy = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.login1",
        "/org/freedesktop/login1/session/auto",
        "org.freedesktop.login1.Session",
    )?;
    // SetBrightness(subsystem: str, name: str, brightness: u32)
    proxy.call::<_, _, ()>("SetBrightness", &("backlight", device, value))?;
    Ok(())
}
