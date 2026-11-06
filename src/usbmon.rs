//! Reads USB events from the kernel's usbmon binary interface.
//!
//! Only the 64-byte event header is fetched: `alloc = 0` means payloads are
//! never copied out, so this sees traffic sizes and never what was typed.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::sync::{Arc, Mutex};

pub const DEVICE: &str = "/dev/usbmon0";
const MON_IOCX_GETX: libc::c_ulong = 0x4018_920A;
const MON_IOCG_STATS: libc::c_ulong = 0x8008_9203;
const MON_IOCT_RING_SIZE: libc::c_ulong = 0x9204;
const RING_BYTES: libc::c_int = 4 << 20;
const CONTROL: u8 = 2;

pub type Key = (u16, u8);
pub type Shared = Arc<Mutex<HashMap<Key, Totals>>>;

#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub struct Totals {
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub errors: u64,
    pub last_error: i32,
}

#[repr(C)]
struct GetArg {
    hdr: *mut u8,
    data: *mut u8,
    alloc: usize,
}

/// True for failures on the wire. Cancels and shutdown are routine, and a
/// control STALL is how a device says "not supported".
fn is_error(status: i32, xfer_type: u8) -> bool {
    match -status {
        n if n <= 0 => false,
        libc::ENOENT | libc::ECONNRESET | libc::ESHUTDOWN | libc::EINPROGRESS => false,
        libc::EPIPE => xfer_type != CONTROL,
        _ => true,
    }
}

/// Adds one raw header to the totals. Callback events carry the real length.
pub fn account(h: &[u8; 64], totals: &mut HashMap<Key, Totals>) {
    let kind = h[8];
    if kind != b'C' && kind != b'E' {
        return;
    }
    let (xfer_type, epnum, devnum) = (h[9], h[10], h[11]);
    let busnum = u16::from_ne_bytes([h[12], h[13]]);
    let status = i32::from_ne_bytes([h[28], h[29], h[30], h[31]]);
    let len = u64::from(u32::from_ne_bytes([h[32], h[33], h[34], h[35]]));
    let t = totals.entry((busnum, devnum)).or_default();
    if is_error(status, xfer_type) {
        t.errors += 1;
        t.last_error = status;
    }
    if kind == b'C' {
        if epnum & 0x80 != 0 {
            t.bytes_in += len;
        } else {
            t.bytes_out += len;
        }
    }
}

pub fn open() -> io::Result<Arc<File>> {
    let f = File::open(DEVICE)?;
    // A bigger ring only reduces drops under load, so a refusal is harmless.
    unsafe { libc::ioctl(f.as_raw_fd(), MON_IOCT_RING_SIZE, RING_BYTES) };
    Ok(Arc::new(f))
}

/// Events the kernel threw away because this reader fell behind.
pub fn dropped(f: &File) -> u32 {
    let mut stats = [0u32; 2];
    let rc = unsafe { libc::ioctl(f.as_raw_fd(), MON_IOCG_STATS, stats.as_mut_ptr()) };
    if rc == 0 { stats[1] } else { 0 }
}

/// Blocks on usbmon forever, adding events to `totals`.
pub fn read_forever(f: Arc<File>, totals: Shared) {
    let mut hdr = [0u8; 64];
    loop {
        let mut arg = GetArg {
            hdr: hdr.as_mut_ptr(),
            data: std::ptr::null_mut(),
            alloc: 0,
        };
        let rc = unsafe { libc::ioctl(f.as_raw_fd(), MON_IOCX_GETX, &raw mut arg) };
        if rc == 0 {
            account(&hdr, &mut totals.lock().unwrap());
        } else if io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return;
        }
    }
}

pub fn status_name(status: i32) -> String {
    match -status {
        libc::EPROTO => "EPROTO -71".into(),
        libc::EPIPE => "stall -32".into(),
        libc::ETIME => "timeout -62".into(),
        libc::ETIMEDOUT => "timeout -110".into(),
        libc::EILSEQ => "CRC -84".into(),
        libc::EOVERFLOW => "babble -75".into(),
        libc::ENODEV => "gone -19".into(),
        n => format!("error -{n}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr(kind: u8, xfer: u8, ep: u8, dev: u8, bus: u16, status: i32, len: u32) -> [u8; 64] {
        let mut h = [0u8; 64];
        h[8] = kind;
        h[9] = xfer;
        h[10] = ep;
        h[11] = dev;
        h[12..14].copy_from_slice(&bus.to_ne_bytes());
        h[28..32].copy_from_slice(&status.to_ne_bytes());
        h[32..36].copy_from_slice(&len.to_ne_bytes());
        h
    }

    #[test]
    fn counts_callbacks_by_direction_and_ignores_submits() {
        let mut m = HashMap::new();
        account(&hdr(b'S', 1, 0x81, 19, 8, -115, 8), &mut m);
        account(&hdr(b'C', 1, 0x81, 19, 8, 0, 8), &mut m);
        account(&hdr(b'C', 3, 0x02, 19, 8, 0, 992), &mut m);
        let t = m[&(8, 19)];
        assert_eq!((t.bytes_in, t.bytes_out, t.errors), (8, 992, 0));
    }

    #[test]
    fn real_errors_count_but_routine_ones_do_not() {
        let mut m = HashMap::new();
        account(&hdr(b'C', 1, 0x81, 2, 8, -libc::EPROTO, 0), &mut m);
        account(&hdr(b'C', 1, 0x81, 2, 8, -libc::ECONNRESET, 0), &mut m);
        account(&hdr(b'C', CONTROL, 0x80, 2, 8, -libc::EPIPE, 0), &mut m);
        account(&hdr(b'C', 1, 0x81, 2, 8, -libc::EPIPE, 0), &mut m);
        let t = m[&(8, 2)];
        assert_eq!(t.errors, 2);
        assert_eq!(t.last_error, -libc::EPIPE);
    }
}
