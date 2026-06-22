// Parsing and bookkeeping for the Pixel `devinfo` partition's A/B boot flags.
//
// Layout (from device/google/gs-common/bootctrl DevInfo.h): 128-byte struct, magic "DEVI",
// then per-slot data. Each slot is 4 bytes at offset 48 (A) / 52 (B):
//   byte 0: retry_count
//   byte 1: bit0 unbootable, bit1 successful, bit2 active, bit3 fastboot_ok
//
// NOTE: editing these flags is *bookkeeping*. The actual slot switch is the UFS boot LUN
// (see `bootlun`); the bootloader rewrites devinfo to match its own choice.

use std::io;

pub const DEVINFO_PATH: &str = "/dev/disk/by-partlabel/devinfo";

const MAGIC: &[u8; 4] = b"DEVI";
const SLOT_A_OFFSET: usize = 48;
const SLOT_LEN: usize = 4;
const MIN_LEN: usize = SLOT_A_OFFSET + 2 * SLOT_LEN; // need both slots' flag bytes

const F_UNBOOTABLE: u8 = 0b0001;
const F_SUCCESSFUL: u8 = 0b0010;
const F_ACTIVE: u8 = 0b0100;
const F_FASTBOOT_OK: u8 = 0b1000;

/// Per-slot boot flags.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SlotData {
    pub retry_count: u8,
    pub unbootable: bool,
    pub successful: bool,
    pub active: bool,
    pub fastboot_ok: bool,
}

impl SlotData {
    pub fn from_bytes(d: &[u8]) -> Self {
        Self {
            retry_count: d[0],
            unbootable: d[1] & F_UNBOOTABLE != 0,
            successful: d[1] & F_SUCCESSFUL != 0,
            active: d[1] & F_ACTIVE != 0,
            fastboot_ok: d[1] & F_FASTBOOT_OK != 0,
        }
    }
}

/// Parsed view of the devinfo header + both slots.
#[derive(Copy, Clone, Debug)]
pub struct Devinfo {
    pub major_version: u16,
    pub minor_version: u16,
    pub slots: [SlotData; 2],
}

fn check(buf: &[u8]) -> io::Result<()> {
    if buf.len() < MIN_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "devinfo too short",
        ));
    }
    if &buf[0..4] != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bad devinfo magic (expected DEVI)",
        ));
    }
    Ok(())
}

impl Devinfo {
    pub fn parse(buf: &[u8]) -> io::Result<Self> {
        check(buf)?;
        Ok(Self {
            major_version: u16::from_le_bytes([buf[4], buf[5]]),
            minor_version: u16::from_le_bytes([buf[6], buf[7]]),
            slots: [
                SlotData::from_bytes(&buf[SLOT_A_OFFSET..SLOT_A_OFFSET + SLOT_LEN]),
                SlotData::from_bytes(&buf[SLOT_A_OFFSET + SLOT_LEN..SLOT_A_OFFSET + 2 * SLOT_LEN]),
            ],
        })
    }
}

/// Byte offset of a slot's 4-byte record (0 = A, 1 = B).
fn slot_offset(slot: usize) -> usize {
    SLOT_A_OFFSET + slot * SLOT_LEN
}

/// In place, make `slot` the active boot slot: retry=7, set ACTIVE, clear UNBOOTABLE, and clear
/// the other slot's ACTIVE bit — mirroring the devinfo bookkeeping in the Tensor boot HAL's
/// setActiveBootSlot.
///
/// SUCCESSFUL is **not** set here (rollback-safe): a freshly-switched slot is unconfirmed, so the
/// bootloader's retry counter counts down and rolls back to the other (still-successful) slot if
/// the new one never boots. Stale SUCCESSFUL on the target is *cleared* so re-flashing a
/// previously-good slot still re-validates it. Call `apply_mark_successful` after a confirmed-good
/// boot to commit it.
///
/// With `mark_successful = true`, set SUCCESSFUL immediately (force-trust — defeats rollback; for
/// manual recovery only). Leaves all other bytes untouched. `slot` must be 0 or 1.
pub fn apply_set_active(buf: &mut [u8], slot: usize, mark_successful: bool) -> io::Result<()> {
    check(buf)?;
    assert!(slot < 2);
    let t = slot_offset(slot);
    let o = slot_offset((slot + 1) % 2);
    buf[t] = 7; // retry_count
    let mut flags = (buf[t + 1] & !F_UNBOOTABLE & !F_SUCCESSFUL) | F_ACTIVE;
    if mark_successful {
        flags |= F_SUCCESSFUL;
    }
    buf[t + 1] = flags;
    buf[o + 1] &= !F_ACTIVE;
    Ok(())
}

/// In place, mark `slot` SUCCESSFUL: set SUCCESSFUL, reset retry=7, clear UNBOOTABLE. Does **not**
/// touch the ACTIVE bit on either slot — call this from the post-boot health service on the
/// *running* slot to commit it, leaving the other slot intact as the rollback target. `slot` must
/// be 0 or 1.
pub fn apply_mark_successful(buf: &mut [u8], slot: usize) -> io::Result<()> {
    check(buf)?;
    assert!(slot < 2);
    let t = slot_offset(slot);
    buf[t] = 7; // reset the retry budget on a confirmed-good boot
    buf[t + 1] = (buf[t + 1] & !F_UNBOOTABLE) | F_SUCCESSFUL;
    Ok(())
}

/// Parse a slot letter to an index (a/A -> 0, b/B -> 1).
pub fn parse_slot(c: char) -> io::Result<usize> {
    match c.to_ascii_lowercase() {
        'a' => Ok(0),
        'b' => Ok(1),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "slot must be a or b",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 128-byte devinfo image resembling the live felix device: magic DEVI, version 3.15,
    // slot A active+successful (retry 7), slot B successful (retry 7), not active.
    fn sample() -> Vec<u8> {
        let mut b = vec![0u8; 128];
        b[0..4].copy_from_slice(b"DEVI");
        b[4..6].copy_from_slice(&3u16.to_le_bytes());
        b[6..8].copy_from_slice(&15u16.to_le_bytes());
        // slot A @48: retry 7, flags = successful|active|fastboot_ok = 0x0e
        b[48] = 7;
        b[49] = 0x0e;
        // slot B @52: retry 7, flags = successful|fastboot? use 0x02 (successful only)
        b[52] = 7;
        b[53] = 0x02;
        b
    }

    #[test]
    fn parses_version_and_slots() {
        let d = Devinfo::parse(&sample()).unwrap();
        assert_eq!((d.major_version, d.minor_version), (3, 15));
        assert!(d.slots[0].active && d.slots[0].successful && d.slots[0].retry_count == 7);
        assert!(!d.slots[1].active && d.slots[1].successful);
    }

    #[test]
    fn rejects_bad_magic_and_short() {
        let mut b = sample();
        b[0] = b'X';
        assert!(Devinfo::parse(&b).is_err());
        assert!(Devinfo::parse(&[0u8; 8]).is_err());
    }

    #[test]
    fn set_active_b_is_rollback_safe_by_default() {
        let mut b = sample();
        apply_set_active(&mut b, 1, false).unwrap();
        let d = Devinfo::parse(&b).unwrap();
        // B becomes active with a full retry budget, but NOT successful — it is unconfirmed, so
        // the bootloader can roll back. A loses active but keeps successful (the rollback target).
        assert!(d.slots[1].active && d.slots[1].retry_count == 7 && !d.slots[1].successful);
        assert!(!d.slots[0].active && d.slots[0].successful);
        // Bytes outside the two flag bytes are untouched (magic intact, version intact).
        assert_eq!(&b[0..8], &sample()[0..8]);
    }

    #[test]
    fn set_active_clears_stale_successful_on_target() {
        let mut b = sample();
        b[53] = F_SUCCESSFUL | F_ACTIVE; // B was previously good+active
        apply_set_active(&mut b, 1, false).unwrap();
        // Re-activating unconfirmed must drop the stale successful so it re-validates.
        assert!(!Devinfo::parse(&b).unwrap().slots[1].successful);
    }

    #[test]
    fn set_active_force_trust_sets_successful() {
        let mut b = sample();
        apply_set_active(&mut b, 1, true).unwrap();
        let d = Devinfo::parse(&b).unwrap();
        assert!(d.slots[1].active && d.slots[1].successful && d.slots[1].retry_count == 7);
        assert!(!d.slots[0].active && d.slots[0].successful);
    }

    #[test]
    fn set_active_a_clears_unbootable_without_marking_successful() {
        let mut b = sample();
        b[49] = F_UNBOOTABLE; // pretend A was marked unbootable
        apply_set_active(&mut b, 0, false).unwrap();
        let d = Devinfo::parse(&b).unwrap();
        assert!(d.slots[0].active && !d.slots[0].unbootable && !d.slots[0].successful);
    }

    #[test]
    fn mark_successful_commits_without_touching_active() {
        let mut b = sample();
        apply_set_active(&mut b, 1, false).unwrap(); // switch to B, unconfirmed
        apply_mark_successful(&mut b, 1).unwrap(); // confirmed-good boot
        let d = Devinfo::parse(&b).unwrap();
        assert!(d.slots[1].active && d.slots[1].successful && d.slots[1].retry_count == 7);
        // Must not disturb ACTIVE bits: A stays inactive, B stays active.
        assert!(!d.slots[0].active);
    }

    #[test]
    fn parse_slot_letters() {
        assert_eq!(parse_slot('a').unwrap(), 0);
        assert_eq!(parse_slot('B').unwrap(), 1);
        assert!(parse_slot('c').is_err());
    }
}
