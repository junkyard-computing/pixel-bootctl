// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Gabriel Marcano, 2025

use std::fs::File;
use std::io;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::path::PathBuf;

use byteorder::ByteOrder;
use byteorder::LittleEndian;

use clap::Parser;

/// Information about the slot
#[allow(clippy::struct_excessive_bools)]
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
        let retry_count = data[0];
        let unbootable = (data[1] & 0b0001) != 0;
        let successful = (data[1] & 0b0010) != 0;
        let active = (data[1] & 0b0100) != 0;
        let fastboot_ok = (data[1] & 0b1000) != 0;
        Self {
            retry_count,
            unbootable,
            successful,
            active,
            fastboot_ok,
        }
    }

    fn write_to<W: Write + ?Sized>(self, writer: &mut W) -> io::Result<()> {
        let mut result = [0u8; 4];
        result[0] = self.retry_count;
        result[1] = u8::from(self.unbootable)
            | u8::from(self.successful) << 1
            | u8::from(self.active) << 2
            | u8::from(self.fastboot_ok) << 3;
        writer.write_all(&result)
    }
}

/// Some of the devinfo partition fields.
///
/// From what I can tell, these are the main fields used for booting purposes, but there seem to be
/// other fields in the binary.
#[derive(Copy, Clone)]
struct Devinfo {
    magic: u32,
    major_version: u16,
    minor_version: u16,
    slots: [SlotData; 2],
}

impl Devinfo {
    fn from(data: &[u8]) -> Self {
        let magic = LittleEndian::read_u32(&data[0..4]);
        let major_version = LittleEndian::read_u16(&data[4..6]);
        let minor_version = LittleEndian::read_u16(&data[6..8]);
        let slot_a = SlotData::from(&data[48..52]);
        let slot_b = SlotData::from(&data[52..56]);
        Self {
            magic,
            major_version,
            minor_version,
            slots: [slot_a, slot_b],
        }
    }

    fn write_to<W: Write + Seek + ?Sized>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&self.magic.to_le_bytes())?;
        writer.write_all(&self.major_version.to_le_bytes())?;
        writer.write_all(&self.minor_version.to_le_bytes())?;
        writer.seek(SeekFrom::Start(48))?;
        self.slots[0].write_to(writer)?;
        self.slots[1].write_to(writer)
    }
}

#[derive(Parser)]
struct Args {
    /// Input file to parse
    input: PathBuf,
    /// Optional output file to write modification, successful bit is set, unbootable is cleared,
    /// and retries are set to 7.
    output: Option<PathBuf>,
    #[arg(short, long)]
    slot: Option<char>,
    #[arg(short, long)]
    retries: Option<u8>,
    #[arg(short = 'S', long)]
    successful: Option<bool>,
    #[arg(short, long)]
    unbootable: Option<bool>,
    #[arg(short, long)]
    fastboot_ok: Option<bool>,
}

// clippy complains about dead_code even though std::process:Termination uses Debug...
#[derive(Debug)]
enum Error {
    #[allow(dead_code)]
    Io(io::Error),
    #[allow(dead_code)]
    Devinfo(String),
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

fn main() -> Result<(), Error> {
    let args = Args::parse();
    let path = &args.input;

    let mut file = File::open(path)?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;
    if &buffer[0..4] != b"DEVI" {
        return Err(Error::Devinfo("Not a devinfo partition".to_string()));
    }
    let devinfo = Devinfo::from(&buffer);
    println!(
        "Version {}.{}",
        devinfo.major_version, devinfo.minor_version
    );

    let active_slot = usize::from(!devinfo.slots[0].active);
    let mut slot_counter: u8 = 65;
    for slot in devinfo.slots {
        println!("slot: {}", slot_counter as char);
        println!("    retry count: {}", slot.retry_count);
        println!("    successful: {}", slot.successful);
        println!("    unbootable: {}", slot.unbootable);
        println!("    active: {}", slot.active);
        println!("    fastboot ok: {}", slot.fastboot_ok);
        slot_counter += 1;
    }

    if let Some(output) = args.output {
        let mut devinfo_copy = devinfo;

        let active_slot = args
            .slot
            .map_or(Ok(active_slot), |active_slot| match active_slot {
                'A' => Ok(0),
                'B' => Ok(1),
                _ => Err(Error::Devinfo(format!("Invalid slot given: {active_slot}"))),
            })?;

        // Update active slot, and deactivate the other one...
        // This assumes there are only two slots!
        devinfo_copy.slots[active_slot].active = true;
        devinfo_copy.slots[(active_slot + 1) % 2].active = false;

        if let Some(retry_count) = args.retries {
            devinfo_copy.slots[active_slot].retry_count = retry_count;
        }

        if let Some(successful) = args.successful {
            devinfo_copy.slots[active_slot].successful = successful;
        }

        if let Some(unbootable) = args.unbootable {
            devinfo_copy.slots[active_slot].unbootable = unbootable;
        }

        if let Some(fastboot_ok) = args.fastboot_ok {
            devinfo_copy.slots[active_slot].fastboot_ok = fastboot_ok;
        }

        let mut output = File::create(output)?;
        // There are a lot of extra fields, effectively copy everything over to the new file first
        output.write_all(&buffer)?;
        // And now write any adjustments to the devinfo fields
        output.seek(SeekFrom::Start(0))?;
        devinfo_copy.write_to(&mut output)?;
    }
    Ok(())
}
