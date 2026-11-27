//! /dev/i2c-N, used the way ddcutil uses it so the two can share a bus.
//!
//! ddcutil 2.x takes `flock(LOCK_EX|LOCK_NB)` on the device node around each
//! exchange (seen in strace, 2026-11-27) and talks through the `I2C_RDWR`
//! ioctl. Taking the same lock keeps a poll from landing in the middle of a
//! ddcutil transaction; a `getvcp SCAN` holds it for tens of seconds, so a
//! held lock means "skip this round", never "wait". `I2C_RDWR` rather than
//! `I2C_SLAVE` + read/write: `I2C_SLAVE` fails with EBUSY on any address a
//! kernel driver has claimed, and needs a second ioctl per address.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::os::fd::AsRawFd;

use anyhow::{Context, Result};

/// struct i2c_msg, include/uapi/linux/i2c.h.
#[repr(C)]
struct I2cMsg {
    addr: u16,
    flags: u16,
    len: u16,
    buf: *mut u8,
}

/// struct i2c_rdwr_ioctl_data, include/uapi/linux/i2c-dev.h.
#[repr(C)]
struct I2cRdwrData {
    msgs: *mut I2cMsg,
    nmsgs: u32,
}

const I2C_RDWR: u32 = 0x0707;
const I2C_M_RD: u16 = 0x0001;

pub enum Op<'a> {
    Write(&'a [u8]),
    Read(&'a mut [u8]),
}

pub struct Bus {
    file: File,
}

impl Bus {
    /// Open /dev/i2c-N and take its lock. `None` when another process holds
    /// the lock.
    pub fn open(n: u32) -> Result<Option<Bus>> {
        let path = format!("/dev/i2c-{n}");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening {path}"))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Bus { file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(e).with_context(|| format!("locking {path}")),
        }
    }

    /// One I2C_RDWR call: the ops run back to back with repeated starts.
    pub fn transfer(&self, addr: u16, ops: &mut [Op<'_>]) -> io::Result<()> {
        let mut msgs = Vec::with_capacity(ops.len());
        for op in ops.iter_mut() {
            let (flags, len, buf) = match op {
                // The kernel only copies from a write buffer, so handing it a
                // *mut made from a shared slice writes nothing through it.
                Op::Write(d) => (0, d.len(), d.as_ptr().cast_mut()),
                Op::Read(b) => (I2C_M_RD, b.len(), b.as_mut_ptr()),
            };
            let len = u16::try_from(len)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "I2C message too long"))?;
            msgs.push(I2cMsg {
                addr,
                flags,
                len,
                buf,
            });
        }
        let mut data = I2cRdwrData {
            msgs: msgs.as_mut_ptr(),
            nmsgs: msgs.len() as u32,
        };
        // SAFETY: `data` points at `msgs`, whose entries point into the slices
        // borrowed by `ops`, each with its true length; all of them outlive
        // this call, and the kernel writes only into the Read buffers.
        let rc = unsafe { libc::ioctl(self.file.as_raw_fd(), I2C_RDWR as _, &mut data) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
