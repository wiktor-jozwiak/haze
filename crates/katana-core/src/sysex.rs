//! Roland SysEx framing for the KATANA MkII (model id 00 00 00 33).
//!
//! Addresses on the wire are 4 × 7-bit bytes. Internally we use *linear* addresses
//! (the 28-bit value those bytes encode) so offsets can be added with plain `+`.

pub const ROLAND: u8 = 0x41;
pub const MODEL_ID: [u8; 4] = [0x00, 0x00, 0x00, 0x33];
pub const RQ1: u8 = 0x11;
pub const DT1: u8 = 0x12;

/// Linear address of a 7-bit-notation address such as `0x6000_0000`.
pub const fn lin(addr7: u32) -> u32 {
    ((addr7 & 0x7f00_0000) >> 3) | ((addr7 & 0x007f_0000) >> 2) | ((addr7 & 0x0000_7f00) >> 1) | (addr7 & 0x7f)
}

/// 4 wire bytes of a linear address.
pub const fn addr_bytes(lin: u32) -> [u8; 4] {
    [((lin >> 21) & 0x7f) as u8, ((lin >> 14) & 0x7f) as u8, ((lin >> 7) & 0x7f) as u8, (lin & 0x7f) as u8]
}

pub const fn from_bytes(b: [u8; 4]) -> u32 {
    ((b[0] as u32) << 21) | ((b[1] as u32) << 14) | ((b[2] as u32) << 7) | b[3] as u32
}

/// Temporary (edit buffer / currently sounding) patch.
pub const TEMP_PATCH: u32 = lin(0x6000_0000);
/// User patch slot: 0 = PANEL, 1..=4 = A:CH1-4, 5..=8 = B:CH1-4.
pub const fn user_patch(slot: u8) -> u32 {
    lin(0x1000_0000) + (slot as u32) * lin(0x0001_0000)
}
pub const CURRENT_PATCH_NUMBER: u32 = lin(0x0001_0000);
pub const EDITOR_COMMUNICATION_LEVEL: u32 = lin(0x7f00_0000);
pub const EDITOR_COMMUNICATION_MODE: u32 = lin(0x7f00_0001);
pub const PATCH_SELECT: u32 = lin(0x7f00_0100);
pub const PATCH_WRITE: u32 = lin(0x7f00_0104);

pub const SLOT_COUNT: u8 = 9;
pub const SLOT_LABELS: [&str; 9] = ["PANEL", "A1", "A2", "A3", "A4", "B1", "B2", "B3", "B4"];

fn checksum(bytes: &[u8]) -> u8 {
    let sum: u32 = bytes.iter().map(|&b| b as u32).sum();
    ((128 - (sum % 128)) % 128) as u8
}

fn frame(dev: u8, cmd: u8, body: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(body.len() + 10);
    m.extend_from_slice(&[0xF0, ROLAND, dev]);
    m.extend_from_slice(&MODEL_ID);
    m.push(cmd);
    m.extend_from_slice(body);
    m.push(checksum(body));
    m.push(0xF7);
    m
}

pub fn dt1(dev: u8, addr: u32, data: &[u8]) -> Vec<u8> {
    let mut body = addr_bytes(addr).to_vec();
    body.extend_from_slice(data);
    frame(dev, DT1, &body)
}

pub fn rq1(dev: u8, addr: u32, size: u32) -> Vec<u8> {
    let mut body = addr_bytes(addr).to_vec();
    body.extend_from_slice(&addr_bytes(size));
    frame(dev, RQ1, &body)
}

pub const IDENTITY_REQUEST: [u8; 6] = [0xF0, 0x7E, 0x7F, 0x06, 0x01, 0xF7];

/// Device id from an Identity Reply if it is a KATANA (family 0x33).
pub fn parse_identity(msg: &[u8]) -> Option<u8> {
    (msg.len() >= 8 && msg[0] == 0xF0 && msg[1] == 0x7E && msg[3] == 0x06 && msg[4] == 0x02 && msg[5] == ROLAND && msg[6] == 0x33)
        .then_some(msg[2])
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dt1 {
    pub addr: u32,
    pub data: Vec<u8>,
}

/// Parse an incoming DT1; returns None for anything else or a bad checksum.
pub fn parse_dt1(msg: &[u8]) -> Option<Dt1> {
    if msg.len() < 14 || msg[0] != 0xF0 || msg[1] != ROLAND || msg[3..7] != MODEL_ID || msg[7] != DT1 || *msg.last()? != 0xF7 {
        return None;
    }
    let body = &msg[8..msg.len() - 2];
    if checksum(body) != msg[msg.len() - 2] {
        return None;
    }
    Some(Dt1 { addr: from_bytes([body[0], body[1], body[2], body[3]]), data: body[4..].to_vec() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addr_roundtrip() {
        assert_eq!(addr_bytes(TEMP_PATCH), [0x60, 0, 0, 0]);
        assert_eq!(addr_bytes(user_patch(8)), [0x10, 0x08, 0, 0]);
        // 7-bit carry: 0x0100 + 0x7f + 1
        assert_eq!(addr_bytes(lin(0x0000_017f) + 1), [0, 0, 0x02, 0x00]);
    }

    #[test]
    fn rq1_matches_captured() {
        // Verified against the real amp: request of the current patch name.
        let m = rq1(0x00, TEMP_PATCH, 16);
        assert_eq!(m, vec![0xF0, 0x41, 0x00, 0, 0, 0, 0x33, 0x11, 0x60, 0, 0, 0, 0, 0, 0, 0x10, 0x10, 0xF7]);
    }

    #[test]
    fn dt1_parse_captured_reply() {
        // Captured reply: "Bohemian Rhapsod"
        let raw = "F0 41 00 00 00 00 33 12 60 00 00 00 42 6F 68 65 6D 69 61 6E 20 52 68 61 70 73 6F 64 0C F7";
        let b: Vec<u8> = raw.split(' ').map(|h| u8::from_str_radix(h, 16).unwrap()).collect();
        let d = parse_dt1(&b).unwrap();
        assert_eq!(d.addr, TEMP_PATCH);
        assert_eq!(d.data, b"Bohemian Rhapsod");
        assert_eq!(dt1(0, TEMP_PATCH, b"Bohemian Rhapsod"), b);
    }

    #[test]
    fn identity() {
        let r = [0xF0, 0x7E, 0x00, 0x06, 0x02, 0x41, 0x33, 0x03, 0, 0, 5, 0, 0, 0, 0xF7];
        assert_eq!(parse_identity(&r), Some(0));
    }
}
