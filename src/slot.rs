// Resolve the *running* A/B slot — whatever the bootloader actually booted, exposed to userspace
// as `androidboot.slot_suffix` in /proc/bootconfig (and /proc/cmdline). This is the authoritative
// "current slot" for the per-boot mark-successful step: it reflects the slot that is really
// running, independent of devinfo's own (bootloader-rewritten) active flag.

use std::io;

const BOOTCONFIG: &str = "/proc/bootconfig";
const CMDLINE: &str = "/proc/cmdline";

/// Extract the running slot index from a bootconfig/cmdline string containing
/// `androidboot.slot_suffix = "_a"` (bootconfig) or `androidboot.slot_suffix=_a` (cmdline).
pub fn parse_slot_suffix(s: &str) -> Option<usize> {
    const KEY: &str = "slot_suffix";
    let idx = s.find(KEY)?;
    // Search AFTER the key (the key itself contains a '_') for the first _a / _b token.
    let after = &s[idx + KEY.len()..];
    let pos = after.find('_')?;
    match after.as_bytes().get(pos + 1) {
        Some(b'a') => Some(0),
        Some(b'b') => Some(1),
        _ => None,
    }
}

/// Read the current (running) slot from the kernel (`/proc/bootconfig`, then `/proc/cmdline`).
pub fn current() -> io::Result<usize> {
    for path in [BOOTCONFIG, CMDLINE] {
        if let Ok(s) = std::fs::read_to_string(path)
            && let Some(slot) = parse_slot_suffix(&s)
        {
            return Ok(slot);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "could not determine the running slot from androidboot.slot_suffix; pass --slot <a|b>",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bootconfig_form() {
        assert_eq!(
            parse_slot_suffix("androidboot.slot_suffix = \"_a\"\n"),
            Some(0)
        );
        assert_eq!(
            parse_slot_suffix("x\nandroidboot.slot_suffix = \"_b\"\ny"),
            Some(1)
        );
    }

    #[test]
    fn parses_cmdline_form() {
        assert_eq!(
            parse_slot_suffix("console=ttynull androidboot.slot_suffix=_b root=/dev/x"),
            Some(1)
        );
    }

    #[test]
    fn none_when_absent_or_bad() {
        // The key itself contains an underscore; make sure we don't latch onto it.
        assert_eq!(parse_slot_suffix("no suffix here"), None);
        assert_eq!(parse_slot_suffix("slot_suffix = \"_c\""), None);
    }
}
