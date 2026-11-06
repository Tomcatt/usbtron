//! User options: the menu's entries and ~/.config/usbtron/usbtron.conf.

use std::ffi::CStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const THEMES: [&str; 3] = ["tron", "ember", "mono"];
const SPEEDS: [u64; 5] = [100, 250, 500, 1000, 2000];
pub const ITEMS: usize = 9;

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub tick_ms: u64,
    pub theme: usize,
    pub bits: bool,
    pub log_scale: bool,
    pub hide_idle: bool,
    pub total: bool,
    pub buses: bool,
    pub detail: bool,
    pub events: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            tick_ms: 500,
            theme: 0,
            bits: false,
            log_scale: false,
            hide_idle: false,
            total: true,
            buses: true,
            detail: true,
            events: true,
        }
    }
}

fn on_off(b: bool) -> String {
    if b { "on".into() } else { "off".into() }
}

impl Options {
    pub fn label(i: usize) -> &'static str {
        [
            "Update speed",
            "Theme",
            "Units",
            "Graph scale",
            "Hide idle devices",
            "Total box",
            "Buses box",
            "Detail box",
            "Events box",
        ][i]
    }

    pub fn value(&self, i: usize) -> String {
        match i {
            0 => format!("{} ms", self.tick_ms),
            1 => THEMES[self.theme].into(),
            2 => {
                if self.bits {
                    "bits".into()
                } else {
                    "bytes".into()
                }
            }
            3 => {
                if self.log_scale {
                    "log".into()
                } else {
                    "auto".into()
                }
            }
            4 => on_off(self.hide_idle),
            5 => on_off(self.total),
            6 => on_off(self.buses),
            7 => on_off(self.detail),
            _ => on_off(self.events),
        }
    }

    /// Steps option `i` forward or back, wrapping around.
    pub fn change(&mut self, i: usize, forward: bool) {
        let step = |cur: usize, len: usize| {
            if forward {
                (cur + 1) % len
            } else {
                (cur + len - 1) % len
            }
        };
        match i {
            0 => {
                let cur = SPEEDS.iter().position(|s| *s == self.tick_ms).unwrap_or(2);
                self.tick_ms = SPEEDS[step(cur, SPEEDS.len())];
            }
            1 => self.theme = step(self.theme, THEMES.len()),
            2 => self.bits = !self.bits,
            3 => self.log_scale = !self.log_scale,
            4 => self.hide_idle = !self.hide_idle,
            5 => self.total = !self.total,
            6 => self.buses = !self.buses,
            7 => self.detail = !self.detail,
            _ => self.events = !self.events,
        }
    }

    fn to_text(&self) -> String {
        format!(
            "tick_ms={}\ntheme={}\nbits={}\nlog_scale={}\nhide_idle={}\ntotal={}\nbuses={}\ndetail={}\nevents={}\n",
            self.tick_ms,
            THEMES[self.theme],
            self.bits,
            self.log_scale,
            self.hide_idle,
            self.total,
            self.buses,
            self.detail,
            self.events
        )
    }

    /// Bad or unknown lines keep their defaults: a hand edit must not stop the app.
    fn from_text(text: &str) -> Options {
        let mut o = Options::default();
        for (k, v) in text.lines().filter_map(|l| l.split_once('=')) {
            let v = v.trim();
            let flag = v.parse::<bool>();
            match (k.trim(), flag) {
                ("tick_ms", _) => {
                    if let Some(s) = v.parse().ok().filter(|s| SPEEDS.contains(s)) {
                        o.tick_ms = s;
                    }
                }
                ("theme", _) => o.theme = THEMES.iter().position(|t| *t == v).unwrap_or(0),
                ("bits", Ok(b)) => o.bits = b,
                ("log_scale", Ok(b)) => o.log_scale = b,
                ("hide_idle", Ok(b)) => o.hide_idle = b,
                ("total", Ok(b)) => o.total = b,
                ("buses", Ok(b)) => o.buses = b,
                ("detail", Ok(b)) => o.detail = b,
                ("events", Ok(b)) => o.events = b,
                _ => {}
            }
        }
        o
    }

    pub fn load() -> Options {
        fs::read_to_string(path()).map_or_else(|_| Options::default(), |t| Options::from_text(&t))
    }

    pub fn save(&self) -> io::Result<()> {
        write_owned(&path(), &self.to_text())
    }
}

/// The real user's uid and gid when running under sudo.
fn sudo_ids() -> Option<(u32, u32)> {
    let get = |k| std::env::var(k).ok()?.parse().ok();
    Some((get("SUDO_UID")?, get("SUDO_GID")?))
}

/// usbtron runs as root, but its config belongs to the person who started it.
fn path() -> PathBuf {
    let home = sudo_ids()
        .and_then(|(uid, _)| {
            let pw = unsafe { libc::getpwuid(uid) };
            (!pw.is_null()).then(|| {
                unsafe { CStr::from_ptr((*pw).pw_dir) }
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .or_else(|| std::env::var("HOME").ok())
        .unwrap_or_else(|| ".".into());
    PathBuf::from(home).join(".config/usbtron/usbtron.conf")
}

fn write_owned(file: &Path, text: &str) -> io::Result<()> {
    let dir = file.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    fs::write(file, text)?;
    if let Some((uid, gid)) = sudo_ids() {
        std::os::unix::fs::chown(dir, Some(uid), Some(gid))?;
        std::os::unix::fs::chown(file, Some(uid), Some(gid))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_survives_junk() {
        let mut o = Options::default();
        for i in 0..ITEMS {
            o.change(i, true);
        }
        assert_eq!(Options::from_text(&o.to_text()), o);
        let junk = Options::from_text("tick_ms=7\ntheme=nope\nbits=maybe\nwhat\nhide_idle=true\n");
        assert_eq!(
            junk,
            Options {
                hide_idle: true,
                ..Options::default()
            }
        );
    }

    #[test]
    fn stepping_wraps_both_ways() {
        let mut o = Options::default();
        o.change(1, false);
        assert_eq!(o.value(1), "mono");
        o.change(1, true);
        assert_eq!(o.value(1), "tron");
        o.tick_ms = 2000;
        o.change(0, true);
        assert_eq!(o.tick_ms, 100);
    }
}
