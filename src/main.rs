// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Gabriel Marcano, 2025
// pixel-bootctl: A/B boot-slot control for a Pixel running Linux.
//
// Renamed/rescoped from pixel-devinfo. devinfo holds A/B bookkeeping flags, but the real
// slot switch on Tensor/felix is the UFS *boot LUN* selection.
//
// MECHANISM (2026-06-18, confirmed on felix from device/google/gs-common/bootctrl source +
// live test): setActiveBootSlot writes the UFS boot-LUN attribute via the Pixel kernel sysfs
// node `/sys/devices/platform/<ufs>/pixel/boot_lun_enabled` ("1"=slot A, "2"=slot B), and
// updates the devinfo active/successful/retry flags as bookkeeping. No fastboot, no keys, no
// GSA/Trusty needed — it's a plain root sysfs write. We switched B->A this way from Debian and
// the device booted slot_suffix=_a. (Trusty probe/send below were the dead-end before we found
// the HAL source — kept as diagnostics.)

use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::RawFd;
use std::path::PathBuf;

use byteorder::{ByteOrder, LittleEndian};
use clap::{Parser, Subcommand};

const DEFAULT_DEVINFO: &str = "/dev/disk/by-partlabel/devinfo";
const DEFAULT_TRUSTY_DEV: &str = "/dev/trusty-ipc-dev0";

/// Per-slot boot flags as stored in devinfo (bytes 48..56, 4 bytes/slot).
#[derive(Copy, Clone)]
struct SlotData {
    retry_count: u8,
    unbootable: bool,
    successful: bool,
    active: bool,
    fastboot_ok: bool,
}

impl SlotData {
    fn from(data: &[u8]) -> Self {
        Self {
            retry_count: data[0],
            unbootable: (data[1] & 0b0001) != 0,
            successful: (data[1] & 0b0010) != 0,
            active: (data[1] & 0b0100) != 0,
            fastboot_ok: (data[1] & 0b1000) != 0,
        }
    }
}

#[derive(Copy, Clone)]
struct Devinfo {
    magic: u32,
    major_version: u16,
    minor_version: u16,
    slots: [SlotData; 2],
}

impl Devinfo {
    fn from(data: &[u8]) -> Self {
        Self {
            magic: LittleEndian::read_u32(&data[0..4]),
            major_version: LittleEndian::read_u16(&data[4..6]),
            minor_version: LittleEndian::read_u16(&data[6..8]),
            slots: [SlotData::from(&data[48..52]), SlotData::from(&data[52..56])],
        }
    }
}

/// Trusty IPC: thin wrapper over /dev/trusty-ipc-dev0 (libtrusty's tipc_connect).
mod trusty {
    use super::{CString, RawFd};

    // #define TIPC_IOC_MAGIC 'r'; TIPC_IOC_CONNECT _IOW('r', 0x80, char*)
    // _IOW => dir=1<<30, size=sizeof(char*)=8<<16, type='r'=0x72<<8, nr=0x80
    // (musl's ioctl request arg is c_int; the value fits in a positive i32)
    const TIPC_IOC_CONNECT: libc::c_int = 0x4008_7280;

    /// Connect to a published Trusty service port. Returns the channel fd on success.
    pub fn connect(dev: &str, port: &str) -> std::io::Result<RawFd> {
        let cdev = CString::new(dev).unwrap();
        let fd = unsafe { libc::open(cdev.as_ptr(), libc::O_RDWR) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let cport = CString::new(port).unwrap();
        let r = unsafe { libc::ioctl(fd, TIPC_IOC_CONNECT, cport.as_ptr()) };
        if r < 0 {
            let e = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(e);
        }
        Ok(fd)
    }

    pub fn close(fd: RawFd) {
        unsafe { libc::close(fd) };
    }

    /// Write a message to a connected channel.
    pub fn send(fd: RawFd, data: &[u8]) -> std::io::Result<usize> {
        let n = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(n as usize)
    }

    /// Read a response with a timeout (ms). Returns the bytes read (possibly empty on timeout).
    pub fn recv(fd: RawFd, timeout_ms: i32, max: usize) -> std::io::Result<Vec<u8>> {
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let p = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if p < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if p == 0 {
            return Ok(Vec::new()); // timeout
        }
        let mut buf = vec![0u8; max];
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        buf.truncate(n as usize);
        Ok(buf)
    }
}

/// Candidate Trusty service ports to probe (boot/AVB/security related first).
const CANDIDATE_PORTS: &[&str] = &[
    "com.android.trusty.boot_control",
    "com.android.trusty.bootcontrol",
    "com.android.trusty.boot",
    "com.android.trusty.avb",
    "com.android.trusty.hwbcc",
    "com.android.trusty.hwkey",
    "com.android.trusty.hwwsk",
    "com.android.trusty.keymaster",
    "com.android.trusty.keymaster.secure",
    "com.android.trusty.keymint",
    "com.android.trusty.gatekeeper",
    "com.android.trusty.secure_storage",
    "com.android.trusty.storage.client.td",
    "com.android.trusty.storage.client.tp",
    "com.android.trusty.storage.client.tdea",
    "com.android.trusty.storage.proxy",
    "com.android.trusty.system_state",
    "com.android.trusty.metrics",
    "com.android.trusty.gsa.hwmgr",
    "com.android.trusty.gsa.hwmgr.tpu",
    "com.android.trusty.gsa.hwmgr.aoc",
    "com.android.trusty.gsa.boot",
    "com.android.trusty.gsa.bootloader",
    "com.google.trusty.gsa.boot_control",
    "com.android.trusty.device_tree",
];

#[derive(Parser)]
#[command(name = "pixel-bootctl", about = "A/B boot-slot control for Pixel-on-Linux")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Read and print A/B slot state from devinfo.
    Status {
        /// devinfo device/file path.
        #[arg(long, default_value = DEFAULT_DEVINFO)]
        devinfo: PathBuf,
    },
    /// Set the active boot slot (a|b): flips the UFS boot LUN + updates devinfo flags.
    SetActiveSlot {
        /// Target slot: a or b.
        slot: char,
        /// devinfo device path.
        #[arg(long, default_value = DEFAULT_DEVINFO)]
        devinfo: PathBuf,
        /// boot_lun_enabled sysfs path (auto-detected if omitted).
        #[arg(long)]
        boot_lun: Option<PathBuf>,
    },
    /// Probe which Trusty service ports accept a connection.
    Probe {
        /// Trusty IPC device node.
        #[arg(long, default_value = DEFAULT_TRUSTY_DEV)]
        dev: String,
        /// Probe a single port instead of the built-in candidate list.
        #[arg(long)]
        port: Option<String>,
    },
    /// Connect to a Trusty port, send hex bytes, print any response (experimental).
    Send {
        #[arg(long, default_value = DEFAULT_TRUSTY_DEV)]
        dev: String,
        /// Trusty service port name.
        #[arg(long)]
        port: String,
        /// Hex payload, e.g. "01000000".
        #[arg(long)]
        hex: String,
        /// Response read timeout (ms).
        #[arg(long, default_value_t = 1000)]
        timeout_ms: i32,
    },
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if s.len() % 2 != 0 {
        return Err("hex must have even length".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

fn cmd_status(path: &PathBuf) -> io::Result<()> {
    let mut buf = Vec::new();
    File::open(path)?.read_to_end(&mut buf)?;
    if buf.len() < 56 || &buf[0..4] != b"DEVI" {
        eprintln!("warning: not a DEVI devinfo image ({} bytes)", buf.len());
    }
    let d = Devinfo::from(&buf);
    println!("devinfo Version {}.{}", d.major_version, d.minor_version);
    for (i, s) in d.slots.iter().enumerate() {
        let name = if i == 0 { 'A' } else { 'B' };
        println!("slot: {name}");
        println!("    retry count: {}", s.retry_count);
        println!("    successful:  {}", s.successful);
        println!("    unbootable:  {}", s.unbootable);
        println!("    active:      {}", s.active);
        println!("    fastboot ok: {}", s.fastboot_ok);
    }
    Ok(())
}

/// Find the Pixel UFS boot_lun_enabled sysfs node (e.g. /sys/devices/platform/14700000.ufs/pixel/...).
fn detect_boot_lun() -> io::Result<PathBuf> {
    let base = "/sys/devices/platform";
    for entry in fs::read_dir(base)? {
        let p = entry?.path();
        if p.file_name().and_then(|s| s.to_str()).is_some_and(|n| n.ends_with(".ufs")) {
            let cand = p.join("pixel/boot_lun_enabled");
            if cand.exists() {
                return Ok(cand);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "boot_lun_enabled not found under /sys/devices/platform/*.ufs/pixel/; pass --boot-lun",
    ))
}

/// Set active slot: flip the UFS boot LUN (the real switch) + update devinfo flags (bookkeeping),
/// mirroring device/google/gs-common/bootctrl setActiveBootSlot.
fn cmd_set_active(slot_char: char, devinfo: &PathBuf, boot_lun: Option<PathBuf>) -> io::Result<()> {
    let slot: usize = match slot_char.to_ascii_lowercase() {
        'a' => 0,
        'b' => 1,
        _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, "slot must be a or b")),
    };

    // 1) The real switch: UFS boot LUN. "1" => slot A, "2" => slot B.
    let lun_path = match boot_lun {
        Some(p) => p,
        None => detect_boot_lun()?,
    };
    let lun_val = if slot == 0 { "1" } else { "2" };
    fs::write(&lun_path, lun_val)?;
    println!("boot LUN: wrote {lun_val} to {}", lun_path.display());

    // 2) Bookkeeping: devinfo flags. target = active+successful, retry=7; other = inactive.
    let mut f = OpenOptions::new().read(true).write(true).open(devinfo)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    if buf.len() < 56 || &buf[0..4] != b"DEVI" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a DEVI devinfo"));
    }
    let (t, o) = (48 + slot * 4, 48 + ((slot + 1) % 2) * 4); // target / other slot offsets
    buf[t] = 7; // retry_count
    buf[t + 1] = (buf[t + 1] & !0b0001) | 0b0010 | 0b0100; // clear unbootable, set successful+active
    buf[o + 1] &= !0b0100; // clear active on the other slot
    f.seek(SeekFrom::Start(t as u64))?;
    f.write_all(&buf[t..t + 2])?;
    f.seek(SeekFrom::Start(o as u64 + 1))?;
    f.write_all(&buf[o + 1..o + 2])?;
    f.sync_all()?;
    println!("devinfo: slot {} marked active+successful (retry 7)", slot_char.to_ascii_uppercase());
    println!("done. reboot to boot slot {}.", slot_char.to_ascii_uppercase());
    Ok(())
}

fn cmd_probe(dev: &str, single: Option<&str>) {
    let ports: Vec<&str> = match single {
        Some(p) => vec![p],
        None => CANDIDATE_PORTS.to_vec(),
    };
    println!("probing {} via {}", ports.len(), dev);
    for port in ports {
        match trusty::connect(dev, port) {
            Ok(fd) => {
                println!("  [CONNECTED] {port}");
                trusty::close(fd);
            }
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(-1);
                // ENOENT/ENODEV/ECONNREFUSED => port not present; EBUSY/EACCES => exists.
                println!("  [   {code:>3} ] {port}  ({e})");
            }
        }
    }
}

fn cmd_send(dev: &str, port: &str, hex: &str, timeout_ms: i32) -> io::Result<()> {
    let payload = hex_decode(hex).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let fd = trusty::connect(dev, port)?;
    println!("connected to {port}, sending {} bytes", payload.len());
    let sent = trusty::send(fd, &payload)?;
    println!("wrote {sent} bytes");
    let resp = trusty::recv(fd, timeout_ms, 4096)?;
    if resp.is_empty() {
        println!("no response (timeout {timeout_ms}ms)");
    } else {
        println!("response ({} bytes): {}", resp.len(), hex_encode(&resp));
    }
    trusty::close(fd);
    Ok(())
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join("")
}

fn main() -> io::Result<()> {
    match Args::parse().cmd {
        Cmd::Status { devinfo } => cmd_status(&devinfo)?,
        Cmd::SetActiveSlot { slot, devinfo, boot_lun } => cmd_set_active(slot, &devinfo, boot_lun)?,
        Cmd::Probe { dev, port } => cmd_probe(&dev, port.as_deref()),
        Cmd::Send { dev, port, hex, timeout_ms } => cmd_send(&dev, &port, &hex, timeout_ms)?,
    }
    Ok(())
}
