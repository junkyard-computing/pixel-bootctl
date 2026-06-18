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

/// In place, mark `slot` active+successful with retry=7 and clear the other slot's active bit,
/// mirroring the devinfo bookkeeping in the Tensor boot HAL's setActiveBootSlot. Leaves all
/// other bytes untouched. `slot` must be 0 or 1.
pub fn apply_set_active(buf: &mut [u8], slot: usize) -> io::Result<()> {
    check(buf)?;
    assert!(slot < 2);
    let t = slot_offset(slot);
    let o = slot_offset((slot + 1) % 2);
    buf[t] = 7; // retry_count
    buf[t + 1] = (buf[t + 1] & !F_UNBOOTABLE) | F_SUCCESSFUL | F_ACTIVE;
    buf[o + 1] &= !F_ACTIVE;
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
    fn set_active_b_flips_flags_only() {
        let mut b = sample();
        apply_set_active(&mut b, 1).unwrap();
        let d = Devinfo::parse(&b).unwrap();
        // B becomes active+successful, retry 7; A loses active but keeps successful.
        assert!(d.slots[1].active && d.slots[1].successful && d.slots[1].retry_count == 7);
        assert!(!d.slots[0].active && d.slots[0].successful);
        // Bytes outside the two flag bytes are untouched (magic intact, version intact).
        assert_eq!(&b[0..8], &sample()[0..8]);
    }

    #[test]
    fn set_active_a_clears_unbootable_and_sets_active() {
        let mut b = sample();
        b[49] = F_UNBOOTABLE; // pretend A was marked unbootable
        apply_set_active(&mut b, 0).unwrap();
        let d = Devinfo::parse(&b).unwrap();
        assert!(d.slots[0].active && d.slots[0].successful && !d.slots[0].unbootable);
    }

    #[test]
    fn parse_slot_letters() {
        assert_eq!(parse_slot('a').unwrap(), 0);
        assert_eq!(parse_slot('B').unwrap(), 1);
        assert!(parse_slot('c').is_err());
    }
}
