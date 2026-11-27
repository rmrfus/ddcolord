//! DDC/CI Get VCP Feature (VESA DDC/CI 1.1, section 4.3; MCCS 2.2).
//!
//! A monitor answers a VCP code it does not implement with a copy of its
//! previous reply rather than an error (BenQ PD3220U, 2026-11-27), so the
//! feature code echoed in the reply is checked against the one requested: a
//! mismatch is exactly that stale copy.

use std::thread::sleep;
use std::time::Duration;

use anyhow::{Result, bail, ensure};

use crate::i2c::{Bus, Op};

/// 7-bit I2C address of the DDC/CI endpoint.
pub const ADDR: u16 = 0x37;
/// Destination byte the host's write checksum starts from (0x37 << 1).
const DEST: u8 = 0x6e;
/// Virtual host address the reply checksum starts from.
const HOST: u8 = 0x50;
const SRC: u8 = 0x51;
const GET_VCP: u8 = 0x01;
const GET_VCP_REPLY: u8 = 0x02;
const REPLY_LEN: usize = 11;

/// The spec asks for 40 ms between a request and reading its reply; NVIDIA's
/// i2c adapters drop the first read now and then at exactly 40.
const REPLY_DELAY: Duration = Duration::from_millis(50);
const ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vcp {
    pub mh: u8,
    pub ml: u8,
    pub sh: u8,
    pub sl: u8,
}

pub fn request(code: u8) -> [u8; 5] {
    let msg = [SRC, 0x80 | 2, GET_VCP, code];
    let chk = msg.iter().fold(DEST, |acc, b| acc ^ b);
    [msg[0], msg[1], msg[2], msg[3], chk]
}

pub fn parse_reply(code: u8, r: &[u8; REPLY_LEN]) -> Result<Vcp> {
    ensure!(r[0] == DEST, "reply from {:#04x}, not the display", r[0]);
    ensure!(
        r[1] & 0x80 != 0,
        "reply length byte {:#04x} lacks 0x80",
        r[1]
    );
    let len = usize::from(r[1] & 0x7f);
    // A zero-length "null message" is how a display says it is busy.
    ensure!(len != 0, "display sent a null message (busy)");
    ensure!(
        len == 8,
        "reply carries {len} bytes, Get VCP replies carry 8"
    );
    let chk = r[..REPLY_LEN - 1].iter().fold(HOST, |acc, b| acc ^ b);
    ensure!(chk == r[REPLY_LEN - 1], "reply checksum mismatch");
    ensure!(
        r[2] == GET_VCP_REPLY,
        "opcode {:#04x} is not a Get VCP reply",
        r[2]
    );
    match r[3] {
        0 => {}
        1 => bail!("display does not support VCP {code:#04x}"),
        rc => bail!("display returned result code {rc:#04x}"),
    }
    ensure!(
        r[4] == code,
        "reply is for VCP {:#04x}, not {code:#04x} (a stale echo)",
        r[4]
    );
    Ok(Vcp {
        mh: r[6],
        ml: r[7],
        sh: r[8],
        sl: r[9],
    })
}

pub fn get_vcp(bus: &Bus, code: u8) -> Result<Vcp> {
    let req = request(code);
    let mut last = None;
    for _ in 0..ATTEMPTS {
        let mut reply = [0u8; REPLY_LEN];
        let got = bus
            .transfer(ADDR, &mut [Op::Write(&req)])
            .and_then(|()| {
                sleep(REPLY_DELAY);
                bus.transfer(ADDR, &mut [Op::Read(&mut reply)])
            })
            .map_err(anyhow::Error::from)
            .and_then(|()| parse_reply(code, &reply));
        match got {
            Ok(v) => return Ok(v),
            Err(e) => last = Some(e),
        }
        sleep(REPLY_DELAY);
    }
    match last {
        Some(e) => Err(e.context(format!("VCP {code:#04x} after {ATTEMPTS} attempts"))),
        None => bail!("VCP {code:#04x}: no attempt made"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(code: u8, sh: u8, sl: u8) -> [u8; REPLY_LEN] {
        let mut r = [DEST, 0x88, GET_VCP_REPLY, 0, code, 0, 0x04, 0x24, sh, sl, 0];
        r[10] = r[..10].iter().fold(HOST, |acc, b| acc ^ b);
        r
    }

    #[test]
    fn request_checksum_matches_the_spec_example() {
        // DDC/CI 1.1 checksum: XOR of 0x6E and every byte sent.
        assert_eq!(request(0x10), [0x51, 0x82, 0x01, 0x10, 0xac]);
    }

    #[test]
    fn the_pd3220u_display_p3_reply_decodes() {
        let v = parse_reply(0xdc, &reply(0xdc, 0x00, 0x22)).unwrap();
        assert_eq!(
            v,
            Vcp {
                mh: 0x04,
                ml: 0x24,
                sh: 0x00,
                sl: 0x22
            }
        );
    }

    #[test]
    fn an_echo_of_another_codes_reply_is_rejected() {
        let err = parse_reply(0xdc, &reply(0x10, 0, 0x3f)).unwrap_err();
        assert!(err.to_string().contains("stale echo"), "{err}");
    }

    #[test]
    fn a_corrupt_checksum_is_rejected() {
        let mut r = reply(0xdc, 0, 0x0a);
        r[9] ^= 0x01;
        assert!(parse_reply(0xdc, &r).is_err());
    }

    #[test]
    fn a_null_message_reads_as_busy() {
        let mut r = [0u8; REPLY_LEN];
        r[..3].copy_from_slice(&[DEST, 0x80, 0xbe]);
        let err = parse_reply(0xdc, &r).unwrap_err();
        assert!(err.to_string().contains("busy"), "{err}");
    }

    #[test]
    fn unsupported_code_is_an_error_not_a_value() {
        let mut r = reply(0xdc, 0, 0);
        r[3] = 1;
        r[10] = r[..10].iter().fold(HOST, |acc, b| acc ^ b);
        assert!(parse_reply(0xdc, &r).is_err());
    }
}
