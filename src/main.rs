// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Gabriel Marcano, 2025
//
// pixel-bootctl: A/B boot-slot control for a Pixel (Tensor) running Linux — the userspace
// analog of Android's `bootctl` / boot_control HAL.
//
// MECHANISM (confirmed on felix from device/google/gs-common/bootctrl + live test):
// setActiveBootSlot writes the UFS boot-LUN attribute via the Pixel kernel sysfs node
// /sys/devices/platform/<ufs>/pixel/boot_lun_enabled ("1"=A, "2"=B) — the real switch — and
// updates the devinfo active/retry flags as bookkeeping. `successful` is set separately, only
// after a confirmed-good boot (markBootSuccessful), so A/B rollback works. No fastboot/keys/Trusty.

mod bootlun;
mod devinfo;
mod hexutil;
mod slot;
mod trusty;

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use devinfo::Devinfo;

#[derive(Parser)]
#[command(
    name = "pixel-bootctl",
    about = "A/B boot-slot control for Pixel-on-Linux"
)]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Read and print A/B slot state from devinfo.
    Status {
        #[arg(long, default_value = devinfo::DEVINFO_PATH)]
        devinfo: PathBuf,
    },
    /// Set the active boot slot (a|b): flips the UFS boot LUN + updates devinfo flags. Rollback-
    /// safe by default — the new slot is marked active but NOT successful, so a slot that never
    /// boots rolls back. Confirm it with `mark-successful` after a good boot.
    SetActiveSlot {
        /// Target slot: a or b.
        slot: char,
        #[arg(long, default_value = devinfo::DEVINFO_PATH)]
        devinfo: PathBuf,
        /// boot_lun_enabled sysfs path (auto-detected if omitted).
        #[arg(long)]
        boot_lun: Option<PathBuf>,
        /// Also mark the slot successful immediately (force-trust; DISABLES rollback). For
        /// manual recovery only — normally let a post-boot health check call `mark-successful`.
        #[arg(long)]
        mark_successful: bool,
    },
    /// Mark the running (or given) slot successful in devinfo — retry=7, successful, clear
    /// unbootable. Does NOT touch the boot LUN.
    MarkSuccessful {
        #[arg(long, default_value = devinfo::DEVINFO_PATH)]
        devinfo: PathBuf,
        /// Slot to mark (a|b); defaults to the running slot (androidboot.slot_suffix).
        #[arg(long)]
        slot: Option<char>,
    },
    /// Probe which Trusty service ports accept a connection (diagnostic).
    Probe {
        #[arg(long, default_value = trusty::DEFAULT_DEV)]
        dev: String,
        /// Probe a single port instead of the built-in candidate list.
        #[arg(long)]
        port: Option<String>,
    },
    /// Connect to a Trusty port, send hex bytes, print any response (diagnostic).
    Send {
        #[arg(long, default_value = trusty::DEFAULT_DEV)]
        dev: String,
        #[arg(long)]
        port: String,
        /// Hex payload, e.g. "01000000".
        #[arg(long)]
        hex: String,
        #[arg(long, default_value_t = 1000)]
        timeout_ms: i32,
    },
}

fn cmd_status(path: &PathBuf) -> io::Result<()> {
    let mut buf = Vec::new();
    File::open(path)?.read_to_end(&mut buf)?;
    let d = Devinfo::parse(&buf)?;
    println!("devinfo Version {}.{}", d.major_version, d.minor_version);
    for (i, s) in d.slots.iter().enumerate() {
        println!("slot: {}", if i == 0 { 'A' } else { 'B' });
        println!("    retry count: {}", s.retry_count);
        println!("    successful:  {}", s.successful);
        println!("    unbootable:  {}", s.unbootable);
        println!("    active:      {}", s.active);
        println!("    fastboot ok: {}", s.fastboot_ok);
    }
    Ok(())
}

fn cmd_set_active(
    slot_char: char,
    devinfo_path: &PathBuf,
    boot_lun: Option<PathBuf>,
    mark_successful: bool,
) -> io::Result<()> {
    let slot = devinfo::parse_slot(slot_char)?;

    // 1) The real switch: UFS boot LUN.
    let lun_path = bootlun::set(slot, boot_lun)?;
    println!(
        "boot LUN: wrote {} to {}",
        bootlun::lun_value(slot),
        lun_path.display()
    );

    // 2) Bookkeeping: devinfo active/retry flags (+ successful only with --mark-successful).
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(devinfo_path)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    devinfo::apply_set_active(&mut buf, slot, mark_successful)?;
    f.seek(SeekFrom::Start(0))?;
    f.write_all(&buf)?;
    f.sync_all()?;
    let su = slot_char.to_ascii_uppercase();
    if mark_successful {
        println!(
            "devinfo: slot {su} marked active+successful (retry 7) — force-trust, NO rollback"
        );
    } else {
        println!(
            "devinfo: slot {su} marked active, NOT successful (retry 7) — rolls back unless a good \
             boot runs `pixel-bootctl mark-successful`"
        );
    }
    println!("done. reboot to boot slot {su}.");
    Ok(())
}

fn cmd_mark_successful(devinfo_path: &PathBuf, slot_override: Option<char>) -> io::Result<()> {
    // The running slot is authoritative; only fall back to it when no override is given.
    let slot = match slot_override {
        Some(c) => devinfo::parse_slot(c)?,
        None => slot::current()?,
    };

    // devinfo bookkeeping ONLY — deliberately no bootlun::set, so the per-boot retry-counter
    // reset keeps working even if the UFS boot_lun_enabled node can't be located.
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(devinfo_path)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    devinfo::apply_mark_successful(&mut buf, slot)?;
    f.seek(SeekFrom::Start(0))?;
    f.write_all(&buf)?;
    f.sync_all()?;
    println!(
        "devinfo: slot {} marked successful (retry 7)",
        if slot == 0 { 'A' } else { 'B' }
    );
    Ok(())
}

fn cmd_probe(dev: &str, single: Option<&str>) {
    let ports: Vec<&str> = match single {
        Some(p) => vec![p],
        None => trusty::CANDIDATE_PORTS.to_vec(),
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
                println!("  [   {code:>3} ] {port}  ({e})");
            }
        }
    }
}

fn cmd_send(dev: &str, port: &str, hex: &str, timeout_ms: i32) -> io::Result<()> {
    let payload =
        hexutil::decode(hex).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let fd = trusty::connect(dev, port)?;
    println!("connected to {port}, sending {} bytes", payload.len());
    println!("wrote {} bytes", trusty::send(fd, &payload)?);
    let resp = trusty::recv(fd, timeout_ms, 4096)?;
    if resp.is_empty() {
        println!("no response (timeout {timeout_ms}ms)");
    } else {
        println!(
            "response ({} bytes): {}",
            resp.len(),
            hexutil::encode(&resp)
        );
    }
    trusty::close(fd);
    Ok(())
}

fn main() -> io::Result<()> {
    match Args::parse().cmd {
        Cmd::Status { devinfo } => cmd_status(&devinfo)?,
        Cmd::SetActiveSlot {
            slot,
            devinfo,
            boot_lun,
            mark_successful,
        } => cmd_set_active(slot, &devinfo, boot_lun, mark_successful)?,
        Cmd::MarkSuccessful { devinfo, slot } => cmd_mark_successful(&devinfo, slot)?,
        Cmd::Probe { dev, port } => cmd_probe(&dev, port.as_deref()),
        Cmd::Send {
            dev,
            port,
            hex,
            timeout_ms,
        } => cmd_send(&dev, &port, &hex, timeout_ms)?,
    }
    Ok(())
}
