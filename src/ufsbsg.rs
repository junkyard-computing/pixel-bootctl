// SPDX-License-Identifier: Apache-2.0
//
// Mainline A/B switch: write the UFS bBootLunEn attribute (query IDN 0x00) via a raw WRITE
// ATTRIBUTE UPIU issued through the ufs-bsg endpoint (/dev/bsg/ufs-bsg0, CONFIG_SCSI_UFS_BSG).
//
// The AOSP Pixel kernel exposes a *writable* sysfs node (pixel/boot_lun_enabled); mainline
// exposes boot_lun_enabled read-only under .../attributes/, so there's nothing to write. But
// the bsg endpoint lets userspace issue the very same WRITE ATTRIBUTE the bootloader honors —
// no fastboot, no keys, no kernel reflash. The request layout mirrors
// drivers/ufs/core/ufshcd.c:ufshcd_query_attr() and include/uapi/scsi/scsi_bsg_ufs.h.

use std::fs::OpenOptions;
use std::io;
use std::os::fd::AsRawFd;

pub const DEV: &str = "/dev/bsg/ufs-bsg0";

const SG_IO: u64 = 0x2285;
const BSG_PROTOCOL_SCSI: u32 = 0;
const BSG_SUB_PROTOCOL_SCSI_TRANSPORT: u32 = 2;

const UPIU_TRANSACTION_QUERY_REQ: u8 = 0x16;
const QUERY_FUNC_WRITE: u8 = 0x81; // UPIU_QUERY_FUNC_STANDARD_WRITE_REQUEST
const OPCODE_WRITE_ATTR: u8 = 0x4;
const IDN_BOOT_LU_EN: u8 = 0x00;

// utp_upiu_header — 3 DW, flat byte layout (second arm of the uapi union).
#[repr(C)]
#[derive(Default)]
struct UtpUpiuHeader {
    transaction_code: u8,
    flags: u8,
    lun: u8,
    task_tag: u8,
    iid_cmd_set_type: u8,
    query_function: u8,
    response: u8,
    status: u8,
    ehs_length: u8,
    device_information: u8,
    data_segment_length: u16,
}

// utp_upiu_query — the query transaction-specific fields (5 DW).
#[repr(C)]
#[derive(Default)]
struct UtpUpiuQuery {
    opcode: u8,
    idn: u8,
    index: u8,
    selector: u8,
    reserved_osf: u16,
    length: u16,
    value: u32, // big-endian on the wire
    reserved: [u32; 2],
}

#[repr(C)]
#[derive(Default)]
struct UtpUpiuReq {
    header: UtpUpiuHeader,
    qr: UtpUpiuQuery,
}

#[repr(C)]
#[derive(Default)]
struct UfsBsgRequest {
    msgcode: u32,
    upiu_req: UtpUpiuReq,
}

#[repr(C)]
#[derive(Default)]
#[allow(dead_code)] // reply fields are populated by the kernel, not all read here
struct UfsBsgReply {
    result: i32,
    reply_payload_rcv_len: u32,
    upiu_rsp: UtpUpiuReq,
}

#[repr(C)]
#[derive(Default)]
#[allow(dead_code)] // sg_io_v4 has many out-only fields we never read
struct SgIoV4 {
    guard: i32,
    protocol: u32,
    subprotocol: u32,
    request_len: u32,
    request: u64,
    request_tag: u64,
    request_attr: u32,
    request_priority: u32,
    request_extra: u32,
    max_response_len: u32,
    response: u64,
    dout_iovec_count: u32,
    dout_xfer_len: u32,
    din_iovec_count: u32,
    din_xfer_len: u32,
    dout_xferp: u64,
    din_xferp: u64,
    timeout: u32,
    flags: u32,
    usr_ptr: u64,
    spare_in: u32,
    driver_status: u32,
    transport_status: u32,
    device_status: u32,
    retry_delay: u32,
    info: u32,
    duration: u32,
    response_len: u32,
    din_resid: i32,
    dout_resid: i32,
    generated_tag: u64,
    spare_out: u32,
    padding: u32,
}

/// bBootLunEn value for a slot index: A (0) -> 1, B (1) -> 2.
fn lun_value(slot: usize) -> u32 {
    if slot == 0 { 1 } else { 2 }
}

/// Switch the active boot LUN for `slot` by issuing a WRITE ATTRIBUTE to bBootLunEn over
/// the ufs-bsg endpoint. Requires CAP_SYS_ADMIN (run as root).
pub fn set(slot: usize) -> io::Result<()> {
    let value = lun_value(slot);

    let mut req = UfsBsgRequest {
        msgcode: UPIU_TRANSACTION_QUERY_REQ as u32,
        ..Default::default()
    };
    req.upiu_req.header.transaction_code = UPIU_TRANSACTION_QUERY_REQ;
    req.upiu_req.header.query_function = QUERY_FUNC_WRITE;
    req.upiu_req.qr.opcode = OPCODE_WRITE_ATTR;
    req.upiu_req.qr.idn = IDN_BOOT_LU_EN;
    req.upiu_req.qr.value = value.to_be();

    let mut rsp = UfsBsgReply::default();

    let mut io_hdr = SgIoV4 {
        guard: b'Q' as i32,
        protocol: BSG_PROTOCOL_SCSI,
        subprotocol: BSG_SUB_PROTOCOL_SCSI_TRANSPORT,
        request_len: std::mem::size_of::<UfsBsgRequest>() as u32,
        request: &req as *const _ as u64,
        max_response_len: std::mem::size_of::<UfsBsgReply>() as u32,
        response: &mut rsp as *mut _ as u64,
        timeout: 5000,
        ..Default::default()
    };

    let f = OpenOptions::new().read(true).write(true).open(DEV)?;
    // SAFETY: io_hdr is a valid, initialized sg_io_v4 living for the call; the kernel copies
    // request/response through the pointers we set. The fd is owned by `f`.
    let rc = unsafe { libc::ioctl(f.as_raw_fd(), SG_IO as _, &mut io_hdr as *mut SgIoV4) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    if rsp.result != 0 {
        return Err(io::Error::other(format!(
            "ufs-bsg WRITE ATTRIBUTE bBootLunEn failed: reply result = {} (0x{:x})",
            rsp.result, rsp.result
        )));
    }
    Ok(())
}
