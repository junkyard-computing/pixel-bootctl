// Thin Trusty IPC wrapper over /dev/trusty-ipc-dev0 (libtrusty's tipc_connect).
//
// These are diagnostics: probing for / poking Trusty service ports. They were the dead-end
// explored before the Tensor boot HAL source revealed that slot switching is the UFS boot-LUN
// sysfs write (see `bootlun`), not a Trusty service. Kept for investigating the TEE surface.

use std::ffi::CString;
use std::os::fd::RawFd;

pub const DEFAULT_DEV: &str = "/dev/trusty-ipc-dev0";

// #define TIPC_IOC_MAGIC 'r'; TIPC_IOC_CONNECT _IOW('r', 0x80, char*)
// _IOW => dir=1<<30, size=sizeof(char*)=8<<16, type='r'=0x72<<8, nr=0x80
// libc::Ioctl is the per-target request type (u64 on x86_64-glibc, i32 on aarch64-musl);
// the value fits both.
pub const TIPC_IOC_CONNECT: libc::Ioctl = 0x4008_7280;

/// Trusty service ports to probe (boot/AVB/security related first).
pub const CANDIDATE_PORTS: &[&str] = &[
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

pub fn send(fd: RawFd, data: &[u8]) -> std::io::Result<usize> {
    let n = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(n as usize)
}

/// Read a response with a timeout (ms). Empty vec on timeout.
pub fn recv(fd: RawFd, timeout_ms: i32, max: usize) -> std::io::Result<Vec<u8>> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let p = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    if p < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if p == 0 {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u8; max];
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    buf.truncate(n as usize);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_constant_matches_iow_r_0x80_charptr() {
        // _IOW('r', 0x80, char*) on 64-bit: dir=1, size=8, type=0x72, nr=0x80
        let expected: libc::Ioctl = (1 << 30) | (8 << 16) | (0x72 << 8) | 0x80;
        assert_eq!(TIPC_IOC_CONNECT, expected);
        assert_eq!(TIPC_IOC_CONNECT, 0x4008_7280);
    }
}
