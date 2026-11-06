//! The USB tree, as sysfs describes it.

use crate::usbmon::Key;
use std::fs;
use std::path::Path;

pub const SYSFS: &str = "/sys/bus/usb/devices";

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub name: String,
    pub bus: u16,
    pub devnum: u8,
    /// Port numbers from the root hub; empty for the root itself.
    pub ports: Vec<u16>,
    pub label: String,
    pub id: String,
    /// Link speed in Mbit/s; low-speed devices report 1.5.
    pub mbps: f64,
    pub usb: String,
    pub hub: bool,
    /// Hub's bDeviceProtocol: 1 = Single TT, 2 = Multi TT.
    pub tt: u8,
}

impl Device {
    pub fn key(&self) -> Key {
        (self.bus, self.devnum)
    }
    pub fn is_inside(&self, ancestor: &Device) -> bool {
        self.bus == ancestor.bus && self.ports.starts_with(&ancestor.ports)
    }
    pub fn depth(&self) -> usize {
        self.ports.len()
    }
}

fn parse_path(name: &str) -> Option<(u16, Vec<u16>)> {
    if name.contains(':') {
        return None;
    }
    if let Some(bus) = name.strip_prefix("usb") {
        return Some((bus.parse().ok()?, vec![]));
    }
    let (bus, rest) = name.split_once('-')?;
    let ports = rest
        .split('.')
        .map(|p| p.parse().ok())
        .collect::<Option<Vec<u16>>>()?;
    Some((bus.parse().ok()?, ports))
}

/// Every device, parents directly before their children.
pub fn scan(root: &Path) -> Vec<Device> {
    let mut out = vec![];
    let Ok(dir) = fs::read_dir(root) else {
        return out;
    };
    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((bus, ports)) = parse_path(&name) else {
            continue;
        };
        let p = entry.path();
        let read = |f: &str| {
            fs::read_to_string(p.join(f))
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        };
        let Ok(devnum) = read("devnum").parse() else {
            continue;
        };
        let product = read("product");
        let id = format!("{}:{}", read("idVendor"), read("idProduct"));
        let label = [product, read("manufacturer"), id.clone()]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or_default();
        let label = if ports.is_empty() {
            format!("Bus {bus}")
        } else {
            label
        };
        out.push(Device {
            bus,
            devnum,
            ports,
            label,
            id,
            mbps: read("speed").parse().unwrap_or(0.0),
            usb: read("version"),
            hub: read("bDeviceClass") == "09",
            tt: read("bDeviceProtocol").parse().unwrap_or(0),
            name,
        });
    }
    out.sort_by(|a, b| (a.bus, &a.ports).cmp(&(b.bus, &b.ports)));
    out
}

/// The nearest high-speed hub above a slower device: the translator whose
/// 12 Mbit/s path it uses. Multi TT gives each port its own path.
pub fn translator<'a>(devs: &'a [Device], d: &Device) -> Option<&'a Device> {
    if d.mbps >= 480.0 {
        return None;
    }
    devs.iter()
        .filter(|h| h.hub && h.mbps == 480.0 && !h.ports.is_empty() && d.is_inside(h))
        .max_by_key(|h| h.depth())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(root: &Path, name: &str, files: &[(&str, &str)]) {
        let p = root.join(name);
        fs::create_dir_all(&p).unwrap();
        for (k, v) in files {
            fs::write(p.join(k), format!("{v}\n")).unwrap();
        }
    }

    #[test]
    fn scans_tree_in_order_and_finds_translator() {
        let tmp = std::env::temp_dir().join(format!("usbtron-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        dev(
            &tmp,
            "usb8",
            &[("devnum", "1"), ("speed", "480"), ("bDeviceClass", "09")],
        );
        dev(
            &tmp,
            "8-4.10",
            &[("devnum", "9"), ("speed", "12"), ("product", "Last")],
        );
        dev(
            &tmp,
            "8-4",
            &[
                ("devnum", "3"),
                ("speed", "480"),
                ("bDeviceClass", "09"),
                ("bDeviceProtocol", "1"),
            ],
        );
        dev(
            &tmp,
            "8-4.2",
            &[("devnum", "5"), ("speed", "12"), ("product", "Gamepad")],
        );
        dev(&tmp, "8-4:1.0", &[("devnum", "99")]);
        dev(
            &tmp,
            "8-6",
            &[
                ("devnum", "7"),
                ("speed", "1.5"),
                ("idVendor", "1234"),
                ("idProduct", "5678"),
            ],
        );
        let d = scan(&tmp);
        fs::remove_dir_all(&tmp).unwrap();
        let names: Vec<_> = d.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["usb8", "8-4", "8-4.2", "8-4.10", "8-6"]);
        assert_eq!((d[4].label.as_str(), d[4].mbps), ("1234:5678", 1.5));
        assert_eq!(d[0].label, "Bus 8");
        assert_eq!(translator(&d, &d[2]).unwrap().name, "8-4");
        assert!(translator(&d, &d[1]).is_none());
    }
}
