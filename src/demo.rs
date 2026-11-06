//! `--demo`: an invented USB tree with synthetic traffic, so the app can be
//! tried without root and shown without exposing anyone's real hardware.

use crate::topo::Device;
use crate::usbmon::{Key, Shared};
use std::thread;
use std::time::{Duration, Instant};

const STEP: Duration = Duration::from_millis(50);
/// Every this many steps the failing device reports a burst of errors.
const ERROR_EVERY: u64 = 240;

type Row = (&'static str, u8, &'static str, f64, bool, u8);

/// Path, device number, label, link speed, is-hub, translator kind.
const TREE: [Row; 21] = [
    ("usb1", 1, "Bus 1", 480.0, true, 1),
    ("1-1", 2, "Powered Hub", 480.0, true, 2),
    ("1-1.1", 3, "Mechanical Keyboard", 12.0, false, 0),
    ("1-1.2", 4, "Gaming Mouse", 12.0, false, 0),
    ("1-1.3", 5, "Flight Stick", 12.0, false, 0),
    ("1-1.4", 6, "Macro Pad", 480.0, false, 0),
    ("1-2", 7, "Old Desk Hub", 480.0, true, 1),
    ("1-2.1", 8, "Legacy Hub", 12.0, true, 0),
    ("1-2.1.1", 9, "Rudder Pedals", 12.0, false, 0),
    ("1-2.1.2", 10, "Throttle", 12.0, false, 0),
    ("1-2.2", 11, "Game Controller", 12.0, false, 0),
    ("1-3", 12, "USB Audio DAC", 480.0, false, 0),
    ("1-4", 13, "HD Webcam", 480.0, false, 0),
    ("usb2", 1, "Bus 2", 5000.0, true, 3),
    ("2-1", 2, "Portable SSD", 5000.0, false, 0),
    ("2-2", 3, "Capture Card", 5000.0, false, 0),
    ("usb3", 1, "Bus 3", 480.0, true, 1),
    ("3-1", 2, "Wireless Dongle", 12.0, false, 0),
    ("3-2", 3, "Card Reader", 480.0, false, 0),
    ("usb4", 1, "Bus 4", 10000.0, true, 3),
    ("4-1", 2, "NVMe Enclosure", 10000.0, false, 0),
];

/// Bytes per second, and how fast the traffic drifts, by device.
const TRAFFIC: [(Key, f64, f64, f64); 10] = [
    ((1, 3), 1_200.0, 0.0, 1.3),
    ((1, 4), 9_000.0, 0.0, 0.7),
    ((1, 5), 3_000.0, 0.0, 1.1),
    ((1, 9), 800.0, 0.0, 0.9),
    ((1, 10), 800.0, 0.0, 0.6),
    ((1, 12), 2_000.0, 1_400_000.0, 0.15),
    ((1, 13), 9_000_000.0, 40_000.0, 0.3),
    ((2, 2), 2_600_000.0, 180_000_000.0, 0.11),
    ((2, 3), 95_000_000.0, 300_000.0, 0.23),
    ((4, 2), 310_000_000.0, 60_000_000.0, 0.07),
];

pub fn devices() -> Vec<Device> {
    TREE.iter()
        .enumerate()
        .map(|(i, (name, devnum, label, mbps, hub, tt))| {
            let (bus, ports) = if let Some(b) = name.strip_prefix("usb") {
                (b, vec![])
            } else {
                let (b, rest) = name.split_once('-').unwrap_or((name, ""));
                (b, rest.split('.').filter_map(|p| p.parse().ok()).collect())
            };
            Device {
                name: (*name).to_string(),
                bus: bus.parse().unwrap_or(0),
                devnum: *devnum,
                ports,
                label: (*label).to_string(),
                id: format!("1234:{i:04x}"),
                mbps: *mbps,
                usb: if *mbps > 480.0 { "3.20" } else { "2.00" }.to_string(),
                hub: *hub,
                tt: *tt,
            }
        })
        .collect()
}

/// Feeds invented totals forever, as the usbmon reader would.
pub fn feed(totals: Shared) {
    let start = Instant::now();
    let dt = STEP.as_secs_f64();
    for step in 0..=u64::MAX {
        let now = start.elapsed().as_secs_f64();
        {
            let mut t = totals.lock().unwrap();
            for (key, input, output, speed) in TRAFFIC {
                let wave = 0.55 + 0.45 * (now * speed).sin() * (now * speed * 0.37).cos();
                let e = t.entry(key).or_default();
                e.bytes_in += (input * wave * dt) as u64;
                e.bytes_out += (output * wave * dt) as u64;
            }
            if step % ERROR_EVERY == ERROR_EVERY - 1 {
                let e = t.entry((1, 9)).or_default();
                e.errors += 3;
                e.last_error = -libc::EPROTO;
            }
        }
        thread::sleep(STEP);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topo::translator;

    #[test]
    fn tree_is_parent_first_and_shows_a_shared_translator() {
        let d = devices();
        for (i, c) in d.iter().enumerate().filter(|(_, c)| c.depth() > 0) {
            let parent = &c.ports[..c.ports.len() - 1];
            assert!(
                d[..i].iter().any(|p| p.bus == c.bus && p.ports == parent),
                "{}",
                c.name
            );
        }
        let pedals = d.iter().find(|x| x.label == "Rudder Pedals").unwrap();
        assert_eq!(translator(&d, pedals).unwrap().label, "Old Desk Hub");
    }
}
