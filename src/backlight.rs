//! The screen's backlight, read from sysfs (`/sys/class/backlight`), for the brightness card:
//! the level of the device userspace should drive, and a watch that says when it changes,
//! whoever changed it (keys, `brightnessctl`, an auto-dimmer, the firmware).

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const ROOT: &str = "/sys/class/backlight";

/// One backlight device.
struct Device {
    dir: PathBuf,
    max: f32,
}

impl Device {
    /// Brightness as a fraction of the maximum. `actual_brightness` is what the hardware
    /// reports, when the driver has it; `brightness` is what was asked for.
    fn level(&self) -> Option<f32> {
        let now = ["actual_brightness", "brightness"]
            .iter()
            .find_map(|f| number(&self.dir.join(f)))?;
        Some((now / self.max).clamp(0.0, 1.0))
    }
}

fn number(path: &Path) -> Option<f32> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|n| n as f32)
}

/// The device to follow: the kernel's own advice is firmware, then platform, then raw (the
/// last driving the panel directly), and the first by name among equals.
fn pick(root: &Path) -> Option<Device> {
    let rank = |dir: &Path| match std::fs::read_to_string(dir.join("type"))
        .unwrap_or_default()
        .trim()
    {
        "firmware" => 0,
        "platform" => 1,
        _ => 2,
    };
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    dirs.sort_by_key(|d| (rank(d), d.clone()));
    dirs.into_iter().find_map(|dir| {
        let max = number(&dir.join("max_brightness")).filter(|m| *m > 0.0)?;
        Some(Device { dir, max })
    })
}

/// Calls `on_change` with the level now and again whenever it changes, on a thread of its own
/// that stops when the returned `Watch` drops. None without a backlight, or without inotify.
pub fn watch(on_change: impl Fn(f32) + Send + 'static) -> Option<Watch> {
    watch_in(Path::new(ROOT), on_change)
}

pub struct Watch {
    stop: Arc<AtomicBool>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn watch_in(root: &Path, on_change: impl Fn(f32) + Send + 'static) -> Option<Watch> {
    let device = pick(root)?;
    // SAFETY: inotify_init1 returns a new descriptor or -1; the OwnedFd takes the only copy.
    let fd = unsafe {
        let raw = libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC);
        if raw < 0 {
            return None;
        }
        OwnedFd::from_raw_fd(raw)
    };
    // Both files: a write through logind or sysfs changes `brightness`; a change the hardware
    // makes on its own shows in `actual_brightness`.
    let mut watching = 0;
    for file in ["brightness", "actual_brightness"] {
        let path = device.dir.join(file);
        if let Ok(c) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) {
            // SAFETY: `c` is a NUL-terminated path that outlives the call.
            let id =
                unsafe { libc::inotify_add_watch(fd.as_raw_fd(), c.as_ptr(), libc::IN_MODIFY) };
            watching += (id >= 0) as u32;
        }
    }
    if watching == 0 {
        return None;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::Builder::new()
        .name("hokum-backlight".into())
        .spawn(move || {
            let mut last = device.level();
            if let Some(l) = last {
                on_change(l);
            }
            let mut buf = [0u8; 1024];
            while !flag.load(Ordering::Relaxed) {
                let mut p = libc::pollfd {
                    fd: fd.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one pollfd, valid for the call. The timeout is how soon a dropped
                // Watch is noticed.
                let ready = unsafe { libc::poll(&mut p, 1, 500) };
                if ready <= 0 {
                    continue;
                }
                // Drain every event queued, then look once: a held key is many writes.
                // SAFETY: reads into `buf`, at most its length.
                while unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) } > 0
                {
                }
                let now = device.level();
                if let Some(l) = now
                    && now != last
                {
                    last = now;
                    on_change(l);
                }
                // A brightness that moves in many small steps settles into one card update.
                std::thread::sleep(Duration::from_millis(20));
            }
        })
        .ok()?;
    Some(Watch { stop })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn device(root: &Path, name: &str, kind: &str, max: u32, now: u32) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("type"), format!("{kind}\n")).unwrap();
        std::fs::write(dir.join("max_brightness"), format!("{max}\n")).unwrap();
        std::fs::write(dir.join("brightness"), format!("{now}\n")).unwrap();
        dir
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hokum-backlight-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_level_is_a_fraction_of_the_maximum() {
        let root = scratch("level");
        let dir = device(&root, "intel_backlight", "raw", 200, 50);
        let d = pick(&root).unwrap();
        assert_eq!(d.level(), Some(0.25));
        std::fs::write(dir.join("actual_brightness"), "100\n").unwrap();
        assert_eq!(d.level(), Some(0.5), "the hardware's figure wins");
        std::fs::write(dir.join("brightness"), "9999\n").unwrap();
        std::fs::remove_file(dir.join("actual_brightness")).unwrap();
        assert_eq!(d.level(), Some(1.0), "never past full");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn firmware_beats_platform_beats_raw() {
        let root = scratch("pick");
        device(&root, "a_raw", "raw", 100, 10);
        device(&root, "b_platform", "platform", 100, 20);
        device(&root, "c_firmware", "firmware", 100, 30);
        assert!(pick(&root).unwrap().dir.ends_with("c_firmware"));
        std::fs::remove_dir_all(root.join("c_firmware")).unwrap();
        assert!(pick(&root).unwrap().dir.ends_with("b_platform"));
        std::fs::remove_dir_all(root.join("b_platform")).unwrap();
        assert!(pick(&root).unwrap().dir.ends_with("a_raw"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_backlight_means_nothing_to_watch() {
        let root = scratch("none");
        assert!(pick(&root).is_none());
        assert!(watch_in(&root, |_| {}).is_none());
        assert!(watch_in(&root.join("missing"), |_| {}).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_watch_reports_the_level_and_each_change() {
        let root = scratch("watch");
        let dir = device(&root, "intel_backlight", "raw", 100, 40);
        let (tx, rx) = mpsc::channel();
        let w = watch_in(&root, move |l| {
            let _ = tx.send(l);
        })
        .expect("a watch");
        let wait = Duration::from_secs(3);
        assert_eq!(
            rx.recv_timeout(wait).unwrap(),
            0.4,
            "the level it starts at"
        );
        std::fs::write(dir.join("brightness"), "70\n").unwrap();
        assert_eq!(rx.recv_timeout(wait).unwrap(), 0.7);
        drop(w);
        let _ = std::fs::remove_dir_all(&root);
    }
}
