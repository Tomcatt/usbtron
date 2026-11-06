//! Turns running totals into per-tick rates and a rolling history.

use crate::opts::{self, Options};
use crate::topo::{self, Device};
use crate::usbmon::{self, Key, Shared, Totals};
use ratatui::layout::Rect;
use ratatui::widgets::TableState;
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const HISTORY: usize = 600;
const LOG_LINES: usize = 200;
const RESCAN: Duration = Duration::from_secs(2);
const IDLE_AFTER_MS: u64 = 30_000;

#[derive(Default, Clone)]
pub struct Series {
    pub input: VecDeque<f64>,
    pub output: VecDeque<f64>,
}

impl Series {
    fn push(&mut self, i: f64, o: f64) {
        for (q, v) in [(&mut self.input, i), (&mut self.output, o)] {
            q.push_back(v);
            if q.len() > HISTORY {
                q.pop_front();
            }
        }
    }
}

#[derive(Default, Clone, Copy, PartialEq, Debug)]
pub struct Rate {
    pub input: f64,
    pub output: f64,
}

pub struct App {
    pub devs: Vec<Device>,
    /// Indexes into `devs` of the rows on screen; the table selects within this.
    pub shown: Vec<usize>,
    pub opts: Options,
    /// Highlighted line while the options menu is open.
    pub menu: Option<usize>,
    pub menu_area: Rect,
    /// Own traffic of each device.
    own: HashMap<Key, Rate>,
    /// Own traffic plus everything below it, indexed like `devs`.
    pub rates: Vec<Rate>,
    pub errors: HashMap<Key, u64>,
    pub hist: HashMap<String, Series>,
    pub total: Series,
    pub log: VecDeque<String>,
    pub table: TableState,
    pub table_area: Rect,
    pub dropped: u32,
    prev: HashMap<Key, Totals>,
    last: Instant,
    rescanned: Instant,
    shared: Shared,
    mon: Option<Arc<File>>,
    /// Where to look for hardware changes; none for the fixed demo tree.
    sysfs: Option<String>,
}

fn clock() -> String {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    unsafe { libc::localtime_r(&raw const now, &raw mut tm) };
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

impl App {
    pub fn new(shared: Shared, mon: Option<Arc<File>>, sysfs: &str) -> App {
        let devs = topo::scan(Path::new(sysfs));
        let mut table = TableState::default();
        table.select(Some(0));
        App {
            rates: vec![Rate::default(); devs.len()],
            shown: (0..devs.len()).collect(),
            devs,
            opts: Options::default(),
            menu: None,
            menu_area: Rect::default(),
            own: HashMap::new(),
            errors: HashMap::new(),
            hist: HashMap::new(),
            total: Series::default(),
            log: VecDeque::new(),
            table,
            table_area: Rect::default(),
            dropped: 0,
            prev: HashMap::new(),
            last: Instant::now(),
            rescanned: Instant::now(),
            shared,
            mon,
            sysfs: Some(sysfs.to_string()),
        }
    }

    /// Shows a fixed device list and stops watching for hardware changes.
    pub fn use_fixed_devices(&mut self, devs: Vec<Device>) {
        self.rates = vec![Rate::default(); devs.len()];
        self.shown = (0..devs.len()).collect();
        self.devs = devs;
        self.sysfs = None;
    }

    pub fn note(&mut self, line: String) {
        self.log.push_front(format!("{} {line}", clock()));
        self.log.truncate(LOG_LINES);
    }

    fn label_of(&self, key: Key) -> String {
        self.devs
            .iter()
            .find(|d| d.key() == key)
            .map_or_else(|| format!("{}:{}", key.0, key.1), |d| d.label.clone())
    }

    /// Replaces the device list, keeping the selection on the same device.
    fn rescan(&mut self) {
        let Some(sysfs) = &self.sysfs else { return };
        let new = topo::scan(Path::new(sysfs));
        let keys = |v: &[Device]| {
            v.iter()
                .map(|d| (d.key(), d.label.clone()))
                .collect::<Vec<_>>()
        };
        let (old_keys, new_keys) = (keys(&self.devs), keys(&new));
        let lines: Vec<String> = old_keys
            .iter()
            .filter(|k| !new_keys.contains(k))
            .map(|k| format!("- {}", k.1))
            .chain(
                new_keys
                    .iter()
                    .filter(|k| !old_keys.contains(k))
                    .map(|k| format!("+ {}", k.1)),
            )
            .collect();
        for l in lines {
            self.note(l);
        }
        let sel = self.selected().map(|d| d.name.clone());
        self.devs = new;
        self.reshow(sel);
    }

    fn sel_index(&self) -> Option<usize> {
        self.shown.get(self.table.selected()?).copied()
    }

    pub fn selected(&self) -> Option<&Device> {
        self.devs.get(self.sel_index()?)
    }

    pub fn selected_rate(&self) -> Rate {
        self.sel_index()
            .and_then(|i| self.rates.get(i))
            .copied()
            .unwrap_or_default()
    }

    /// Recent traffic or any error keeps a row visible, so it doesn't flicker.
    fn busy(&self, d: &Device) -> bool {
        let window = (IDLE_AFTER_MS / self.opts.tick_ms) as usize;
        let moved = |q: &VecDeque<f64>| q.iter().rev().take(window).any(|v| *v > 0.0);
        self.errors.contains_key(&d.key())
            || self
                .hist
                .get(&d.name)
                .is_some_and(|s| moved(&s.input) || moved(&s.output))
    }

    /// Rebuilds the visible rows, keeping `sel` selected if it's still there.
    fn reshow(&mut self, sel: Option<String>) {
        let hide = self.opts.hide_idle;
        self.shown = (0..self.devs.len())
            .filter(|i| !hide || self.busy(&self.devs[*i]))
            .collect();
        let row = sel.and_then(|s| self.shown.iter().position(|i| self.devs[*i].name == s));
        let last = self.shown.len().saturating_sub(1);
        self.table.select(Some(
            row.unwrap_or(self.table.selected().unwrap_or(0).min(last)),
        ));
    }

    /// Changes an option from the menu or a hotkey, and saves it straight away.
    pub fn change(&mut self, item: usize, forward: bool) {
        let sel = self.selected().map(|d| d.name.clone());
        self.opts.change(item, forward);
        self.reshow(sel);
        if let Err(e) = self.opts.save() {
            self.note(format!("! options not saved: {e}"));
        }
    }

    pub fn toggle_menu(&mut self) {
        self.menu = if self.menu.is_some() { None } else { Some(0) };
    }

    pub fn menu_move(&mut self, by: isize) {
        if let Some(m) = self.menu {
            self.menu = Some((m as isize + by).rem_euclid(opts::ITEMS as isize) as usize);
        }
    }

    /// A click on a menu line steps it; a click outside closes the menu.
    pub fn menu_click(&mut self, x: u16, y: u16) {
        let a = self.menu_area;
        if !(x >= a.x && x < a.right() && y >= a.y && y < a.bottom()) {
            self.menu = None;
        } else if y > a.y && ((y - a.y - 1) as usize) < opts::ITEMS {
            let i = (y - a.y - 1) as usize;
            self.menu = Some(i);
            self.change(i, true);
        }
    }

    pub fn tick(&mut self) {
        let dt = self.last.elapsed().as_secs_f64().max(0.001);
        self.last = Instant::now();
        if self.rescanned.elapsed() >= RESCAN {
            self.rescanned = Instant::now();
            self.rescan();
        }
        if let Some(f) = &self.mon {
            self.dropped = usbmon::dropped(f);
        }
        let now = self.shared.lock().unwrap().clone();
        self.step(now, dt);
    }

    fn step(&mut self, now: HashMap<Key, Totals>, dt: f64) {
        self.own.clear();
        let (mut total_in, mut total_out) = (0.0, 0.0);
        for (key, t) in &now {
            let p = self.prev.get(key).copied().unwrap_or_default();
            let r = Rate {
                input: t.bytes_in.saturating_sub(p.bytes_in) as f64 / dt,
                output: t.bytes_out.saturating_sub(p.bytes_out) as f64 / dt,
            };
            total_in += r.input;
            total_out += r.output;
            self.own.insert(*key, r);
            let new_errors = t.errors.saturating_sub(p.errors);
            if new_errors > 0 {
                *self.errors.entry(*key).or_default() += new_errors;
                let line = format!(
                    "! {} {} x{new_errors}",
                    self.label_of(*key),
                    usbmon::status_name(t.last_error)
                );
                self.note(line);
            }
        }
        self.prev = now;
        self.rates = self
            .devs
            .iter()
            .map(|d| {
                self.devs
                    .iter()
                    .filter(|c| c.is_inside(d))
                    .fold(Rate::default(), |a, c| {
                        let r = self.own.get(&c.key()).copied().unwrap_or_default();
                        Rate {
                            input: a.input + r.input,
                            output: a.output + r.output,
                        }
                    })
            })
            .collect();
        for (d, r) in self.devs.iter().zip(&self.rates) {
            self.hist
                .entry(d.name.clone())
                .or_default()
                .push(r.input, r.output);
        }
        self.total.push(total_in, total_out);
        let sel = self.selected().map(|d| d.name.clone());
        self.reshow(sel);
    }

    pub fn move_sel(&mut self, by: isize) {
        let last = self.shown.len().saturating_sub(1) as isize;
        let cur = self.table.selected().unwrap_or(0) as isize;
        self.table.select(Some((cur + by).clamp(0, last) as usize));
    }

    /// Left click on a row of the device table.
    pub fn click(&mut self, x: u16, y: u16) {
        let a = self.table_area;
        const HEADER: u16 = 2;
        if x > a.x && x + 1 < a.right() && y >= a.y + HEADER && y + 1 < a.bottom() {
            let i = (y - a.y - HEADER) as usize + self.table.offset();
            if i < self.shown.len() {
                self.table.select(Some(i));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn d(name: &str, devnum: u8, ports: &[u16]) -> Device {
        Device {
            name: name.into(),
            bus: 8,
            devnum,
            ports: ports.to_vec(),
            label: name.into(),
            id: String::new(),
            mbps: 12.0,
            usb: String::new(),
            hub: false,
            tt: 0,
        }
    }

    #[test]
    fn rates_roll_up_to_parents_and_errors_are_logged() {
        let mut a = App::new(Arc::new(Mutex::new(HashMap::new())), None, "/nonexistent");
        a.devs = vec![
            d("usb8", 1, &[]),
            d("8-4", 3, &[4]),
            d("8-4.1", 5, &[4, 1]),
            d("8-5", 6, &[5]),
        ];
        let t = |b, e| Totals {
            bytes_in: b,
            bytes_out: 0,
            errors: e,
            last_error: -71,
        };
        a.step(
            HashMap::from([((8, 5), t(1000, 0)), ((8, 6), t(500, 0))]),
            1.0,
        );
        a.step(
            HashMap::from([((8, 5), t(3000, 2)), ((8, 6), t(500, 0))]),
            2.0,
        );
        let r: Vec<f64> = a.rates.iter().map(|r| r.input).collect();
        assert_eq!(r, [1000.0, 1000.0, 1000.0, 0.0]);
        assert_eq!(a.errors[&(8, 5)], 2);
        assert!(a.log[0].contains("8-4.1 EPROTO -71 x2"), "{}", a.log[0]);
        assert_eq!(a.total.input.back(), Some(&1000.0));
    }

    #[test]
    fn rescan_logs_plug_and_unplug_and_keeps_selection() {
        let tmp = std::env::temp_dir().join(format!("usbtron-app-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let dev = |name: &str, num: &str, label: &str| {
            let p = tmp.join(name);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("devnum"), num).unwrap();
            std::fs::write(p.join("product"), label).unwrap();
        };
        dev("usb8", "1", "root");
        dev("8-4", "2", "Gamepad");
        let mut a = App::new(
            Arc::new(Mutex::new(HashMap::new())),
            None,
            tmp.to_str().unwrap(),
        );
        a.table.select(Some(1));
        dev("8-2", "3", "Keyboard");
        std::fs::remove_dir_all(tmp.join("usb8")).unwrap();
        a.rescan();
        assert_eq!(a.selected().unwrap().label, "Gamepad");
        assert!(a.log.iter().any(|l| l.ends_with("+ Keyboard")));
        assert!(a.log.iter().any(|l| l.ends_with("- Bus 8")));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn hide_idle_keeps_busy_rows_and_the_selection() {
        let mut a = App::new(Arc::new(Mutex::new(HashMap::new())), None, "/nonexistent");
        a.devs = vec![d("usb8", 1, &[]), d("8-4", 3, &[4]), d("8-5", 6, &[5])];
        a.shown = vec![0, 1, 2];
        a.opts.hide_idle = true;
        a.table.select(Some(1));
        let t = |b| Totals {
            bytes_in: b,
            ..Totals::default()
        };
        a.step(HashMap::from([((8, 3), t(1000))]), 1.0);
        assert_eq!(a.shown, [0, 1], "8-5 never moved a byte");
        assert_eq!(a.selected().unwrap().name, "8-4");
        a.step(HashMap::from([((8, 3), t(1000))]), 1.0);
        assert_eq!(a.shown, [0, 1], "stays listed while recently busy");
        a.opts.hide_idle = false;
        a.reshow(Some("8-5".into()));
        assert_eq!(a.selected().unwrap().name, "8-5");
    }
}
