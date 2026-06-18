// The actual A/B slot switch on Tensor: the UFS boot-LUN attribute, exposed by the Pixel
// kernel as a sysfs file. Writing "1" selects boot LUN A (slot A), "2" selects B.
//   /sys/devices/platform/<ufs>/pixel/boot_lun_enabled
// This is what device/google/gs-common/bootctrl setActiveBootSlot writes; it needs no
// fastboot, keys, GSA, or Trusty.

use std::fs;
use std::io;
use std::path::PathBuf;

const PLATFORM_DIR: &str = "/sys/devices/platform";
const UFS_SUFFIX: &str = ".ufs";
const NODE_REL: &str = "pixel/boot_lun_enabled";

/// sysfs value for a slot index: A (0) -> "1", B (1) -> "2".
pub fn lun_value(slot: usize) -> &'static str {
    if slot == 0 { "1" } else { "2" }
}

/// Find `/sys/devices/platform/<*.ufs>/pixel/boot_lun_enabled`.
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

/// Write the boot LUN for `slot` to `path` (or the auto-detected node). Returns the path used.
pub fn set(slot: usize, path: Option<PathBuf>) -> io::Result<PathBuf> {
    let path = match path {
        Some(p) => p,
        None => detect()?,
    };
    fs::write(&path, lun_value(slot))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lun_value_maps_slots() {
        assert_eq!(lun_value(0), "1"); // A -> boot LUN A
        assert_eq!(lun_value(1), "2"); // B -> boot LUN B
    }
}
