//! The EDID base block fields that name a monitor (VESA E-EDID 1.4, section 3).
//!
//! Only the 128-byte base block is read: the manufacturer ID, and the monitor
//! name and serial string descriptors. Extension blocks carry nothing that
//! identifies the panel.

use anyhow::{Result, bail, ensure};

pub const BASE_LEN: usize = 128;
const HEADER: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edid {
    /// Three-letter PNP manufacturer ID, e.g. "BNQ".
    pub vendor: String,
    /// Monitor name descriptor (tag 0xFC), e.g. "BenQ PD3220U".
    pub name: Option<String>,
    /// Serial string descriptor (tag 0xFF).
    pub serial: Option<String>,
}

pub fn parse(bytes: &[u8]) -> Result<Edid> {
    let Some(base) = bytes.get(..BASE_LEN) else {
        bail!("EDID is {} bytes, shorter than its base block", bytes.len());
    };
    ensure!(base[..8] == HEADER, "EDID header is missing");
    let sum = base.iter().fold(0u8, |acc, b| acc.wrapping_add(*b));
    ensure!(sum == 0, "EDID base block checksum is off by {sum:#04x}");

    // Bytes 8-9, big-endian: three 5-bit letters, 1 = 'A'.
    let id = u16::from_be_bytes([base[8], base[9]]);
    let vendor = [10u16, 5, 0]
        .iter()
        .map(|shift| char::from(b'@' + ((id >> shift) & 0x1f) as u8))
        .collect();

    let mut edid = Edid {
        vendor,
        name: None,
        serial: None,
    };
    // Four 18-byte descriptors from byte 54. A display descriptor starts with
    // a zero pixel clock and a zero byte, then its tag at offset 3.
    for d in base[54..126].as_chunks::<18>().0 {
        if d[..3] != [0, 0, 0] {
            continue;
        }
        let text = descriptor_text(&d[5..]);
        match d[3] {
            0xfc => edid.name = Some(text),
            0xff => edid.serial = Some(text),
            _ => {}
        }
    }
    Ok(edid)
}

/// Descriptor text ends at 0x0A and is padded with spaces.
fn descriptor_text(raw: &[u8]) -> String {
    let end = raw.iter().position(|&b| b == 0x0a).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).trim_end().to_string()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A base block with the given PNP ID, name and serial descriptors and a
    /// correct checksum.
    pub(crate) fn synthetic(vendor: &str, name: &str, serial: &str) -> Vec<u8> {
        let mut b = vec![0u8; BASE_LEN];
        b[..8].copy_from_slice(&HEADER);
        let l: Vec<u16> = vendor.bytes().map(|c| u16::from(c - b'@')).collect();
        b[8..10].copy_from_slice(&((l[0] << 10) | (l[1] << 5) | l[2]).to_be_bytes());
        // Descriptor 1 stays a detailed timing (non-zero pixel clock).
        b[54] = 0x01;
        for (slot, tag, text) in [(72, 0xfc, name), (90, 0xff, serial)] {
            b[slot + 3] = tag;
            let mut field = [b' '; 13];
            field[..text.len()].copy_from_slice(text.as_bytes());
            if text.len() < 13 {
                field[text.len()] = 0x0a;
            }
            b[slot + 5..slot + 18].copy_from_slice(&field);
        }
        let sum = b.iter().fold(0u8, |acc, x| acc.wrapping_add(*x));
        b[127] = 0u8.wrapping_sub(sum);
        b
    }

    #[test]
    fn vendor_name_and_serial_come_out_of_the_descriptors() {
        let e = parse(&synthetic("BNQ", "BenQ PD3220U", "ABC123")).unwrap();
        assert_eq!(e.vendor, "BNQ");
        assert_eq!(e.name.as_deref(), Some("BenQ PD3220U"));
        assert_eq!(e.serial.as_deref(), Some("ABC123"));
    }

    #[test]
    fn a_thirteen_character_name_has_no_terminator_and_still_parses() {
        let e = parse(&synthetic("DEL", "DELL U2723QE1", "X")).unwrap();
        assert_eq!(e.name.as_deref(), Some("DELL U2723QE1"));
    }

    #[test]
    fn a_bad_checksum_is_rejected() {
        let mut b = synthetic("BNQ", "BenQ PD3220U", "ABC123");
        b[127] ^= 1;
        assert!(parse(&b).is_err());
    }

    #[test]
    fn a_truncated_edid_is_rejected_not_indexed_past_the_end() {
        assert!(parse(&[0u8; 64]).is_err());
    }

    #[test]
    fn extension_blocks_after_the_base_are_ignored() {
        let mut b = synthetic("BNQ", "BenQ PD3220U", "ABC123");
        b.extend_from_slice(&[0xaa; 128]);
        assert_eq!(parse(&b).unwrap().vendor, "BNQ");
    }
}
