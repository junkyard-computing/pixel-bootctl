// The actual A/B slot switch on Tensor: the UFS bBootLunEn attribute. Writing "1" selects boot
// LUN A (slot A), "2" selects B. Two backends, autodetected:
//
//   AOSP  — the Pixel kernel exposes a *writable* sysfs node:
//             /sys/devices/platform/<ufs>/pixel/boot_lun_enabled
//           This is what device/google/gs-common/bootctrl setActiveBootSlot writes.
//   Linux — mainline exposes boot_lun_enabled read-only under .../attributes/, so instead we
//           issue the same WRITE ATTRIBUTE over the ufs-bsg endpoint (see ufsbsg.rs).
//
// Neither path needs fastboot, keys, GSA, or Trusty.

use std::fs;
use std::io;
use std::path::PathBuf;

use crate::ufsbsg;

const PLATFORM_DIR: &str = "/sys/devices/platform";
const UFS_SUFFIX: &str = ".ufs";
const NODE_REL: &str = "pixel/boot_lun_enabled";

/// Which backend performs the boot-LUN write.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Autodetect: use the AOSP sysfs node if present, else the mainline ufs-bsg endpoint.
    Auto,
    /// Force the AOSP writable sysfs node.
    Aosp,
    /// Force the mainline ufs-bsg WRITE ATTRIBUTE path.
    Linux,
}

/// sysfs value for a slot index: A (0) -> "1", B (1) -> "2".
pub fn lun_value(slot: usize) -> &'static str {
    if slot == 0 { "1" } else { "2" }
}

/// Find `/sys/devices/platform/<*.ufs>/pixel/boot_lun_enabled` (AOSP-only node).
pub fn detect() -> io::Result<PathBuf> {
    for entry in fs::read_dir(PLATFORM_DIR)? {
        let p = entry?.path();
        if p.file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n.ends_with(UFS_SUFFIX))
        {
            let cand = p.join(NODE_REL);
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

fn set_aosp(slot: usize, path: Option<PathBuf>) -> io::Result<String> {
    let path = match path {
        Some(p) => p,
        None => detect()?,
    };
    fs::write(&path, lun_value(slot))?;
    Ok(format!(
        "boot LUN: wrote {} to {} (AOSP sysfs)",
        lun_value(slot),
        path.display()
    ))
}

fn set_linux(slot: usize) -> io::Result<String> {
    ufsbsg::set(slot)?;
    Ok(format!(
        "boot LUN: wrote {} to {} via ufs-bsg WRITE ATTRIBUTE (mainline)",
        lun_value(slot),
        ufsbsg::DEV
    ))
}

/// Switch the active boot LUN for `slot`. `aosp_path` overrides the AOSP sysfs node location
/// (implies the AOSP backend when `backend` is Auto). Returns a description of what was written.
pub fn set(slot: usize, backend: Backend, aosp_path: Option<PathBuf>) -> io::Result<String> {
    match backend {
        Backend::Aosp => set_aosp(slot, aosp_path),
        Backend::Linux => set_linux(slot),
        Backend::Auto => match aosp_path.or_else(|| detect().ok()) {
            Some(p) => set_aosp(slot, Some(p)),
            None => set_linux(slot),
        },
    }
}

/// Parse a `boot_lun_enabled` value into a slot index. The node is *written* as "1"/"2" but
/// *reads back* as "0x1"/"0x2" on felix, so accept both forms. 1 -> A (0), 2 -> B (1).
pub fn parse_lun_value(s: &str) -> Option<usize> {
    let t = s.trim();
    let t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    match t {
        "1" => Some(0),
        "2" => Some(1),
        _ => None,
    }
}

/// Read the current boot LUN from `path` (or the auto-detected node) and map it to a slot index
/// (A=0, B=1). This is the hardware's view of which slot is selected to boot — the fallback
/// "current slot" source when the kernel exposes no `androidboot.slot_suffix` (mainline boot).
pub fn get(path: Option<PathBuf>) -> io::Result<usize> {
    let path = match path {
        Some(p) => p,
        None => detect()?,
    };
    let raw = fs::read_to_string(&path)?;
    parse_lun_value(&raw).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unexpected boot_lun_enabled value {:?} in {}",
                raw.trim(),
                path.display()
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lun_value_maps_slots() {
        assert_eq!(lun_value(0), "1"); // A -> boot LUN A
        assert_eq!(lun_value(1), "2"); // B -> boot LUN B
    }

    #[test]
    fn parses_lun_value_write_and_read_forms() {
        // Written form ("1"/"2") and the read-back hex form ("0x1"/"0x2"), with whitespace.
        assert_eq!(parse_lun_value("1"), Some(0));
        assert_eq!(parse_lun_value("2\n"), Some(1));
        assert_eq!(parse_lun_value("0x1\n"), Some(0));
        assert_eq!(parse_lun_value(" 0x2 "), Some(1));
        assert_eq!(parse_lun_value("0X2"), Some(1));
    }

    #[test]
    fn rejects_bad_lun_value() {
        assert_eq!(parse_lun_value("0"), None);
        assert_eq!(parse_lun_value("3"), None);
        assert_eq!(parse_lun_value(""), None);
        assert_eq!(parse_lun_value("0xff"), None);
    }
}
