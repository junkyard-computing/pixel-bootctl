# pixel-bootctl

A/B boot-slot control for a **Google Pixel (Tensor) running Linux** — the userspace
equivalent of Android's `bootctl` / `boot_control` HAL, for devices whose Android userspace
has been replaced by a plain Linux distro (e.g. the junkyard Debian-on-Pixel images).

Rescoped from `pixel-devinfo`: that tool only parsed/edited the `devinfo` partition's boot
flags. As it turns out, **editing devinfo does not switch the active slot on Tensor** — so this
tool adds the piece that actually does, and keeps devinfo handling as bookkeeping.

## How slot switching actually works on Tensor

Reading Google's own boot-control HAL (`device/google/gs-common/bootctrl/1.2/BootControl.cpp`),
`setActiveBootSlot()` does **not** use fastboot, signing keys, GSA, or Trusty for the switch. It:

1. Writes the **UFS boot-LUN attribute** through a Pixel-kernel sysfs node:

   ```
   /sys/devices/platform/<ufs>/pixel/boot_lun_enabled    "1" = slot A, "2" = slot B
   ```

   This selects which UFS boot LUN the SoC boots from (`sdb` = slot-A bootloaders, `sdc` =
   slot-B). **This is the real switch** — a plain root-writable `/sys` file.
2. Updates the `devinfo` partition's per-slot `active` / `successful` / `retry` flags as
   bookkeeping (128-byte `DEVI` struct; A/B slot data at offset 48).

`markBootSuccessful` additionally bumps an anti-rollback counter over Trusty, but the slot
*switch itself* needs none of that. This is exactly what LineageOS / `bootctl` do — no keys
involved; slot selection is keyless. The only reason a bare Linux distro can't do it out of the
box is that nobody had identified the sysfs knob.

Verified on a Pixel Fold (felix): switching `boot_lun_enabled` from Linux and rebooting moves
`androidboot.slot_suffix` between `_a` and `_b` in both directions.

## Commands

```
pixel-bootctl status [--devinfo PATH]
        Read and print A/B slot state from devinfo (default /dev/disk/by-partlabel/devinfo).

pixel-bootctl set-active-slot <a|b> [--devinfo PATH] [--boot-lun PATH]
        Set the active boot slot: write the UFS boot LUN (auto-detects the
        /sys/devices/platform/*.ufs/pixel/boot_lun_enabled node) and mark the target slot
        active+successful (retry 7) in devinfo. Reboot to take effect.

pixel-bootctl probe [--dev /dev/trusty-ipc-dev0] [--port NAME]
        Enumerate which Trusty IPC service ports accept a connection (diagnostic).

pixel-bootctl send  --port NAME --hex BYTES [--dev ...] [--timeout-ms N]
        Connect to a Trusty port, send raw bytes, print the response (diagnostic).
```

Run as root (devinfo, the UFS sysfs node, and the Trusty device all require it).

### Example

```sh
# on the device
sudo pixel-bootctl status
sudo pixel-bootctl set-active-slot a
sudo reboot
# after reboot: cat /proc/bootconfig | grep slot_suffix  ->  "_a"
```

## Building

The host needs only Nix (with flakes). The flake cross-compiles a fully static
`aarch64-unknown-linux-musl` binary that runs on the device's Debian as-is.

```sh
nix build           # -> result/bin/pixel-bootctl  (static aarch64)
scp result/bin/pixel-bootctl <device>:/usr/local/bin/pixel-bootctl
```

A `cargo`/`rustc` dev shell is also provided:

```sh
nix develop
cargo build
```

## Safety notes

- `set-active-slot` changes which slot the bootloader boots next. Make sure the target slot is
  actually bootable; the inactive slot is a separate boot chain.
- The switch is reversible from Linux (just set the other slot) **as long as the device boots
  far enough to run this tool**. If a slot is broken and won't boot, recovery needs a host with
  `fastboot`.
- Anti-rollback (`markBootSuccessful`'s Trusty step) is not yet implemented here; only the
  devinfo `successful` flag is set.

## License

Apache-2.0. Originally `pixel-devinfo` by Gabriel Marcano, 2025.
