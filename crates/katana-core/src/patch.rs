//! A complete KATANA MkII patch: raw bytes of all 31 blocks, in `PATCH_BLOCKS` order.

use crate::map::{amp_name, ParamRef, PATCH_BLOCKS};
use crate::sysex;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Patch {
    blocks: Vec<Vec<u8>>,
}

/// Indices into `PATCH_BLOCKS` for the blocks the UI touches constantly.
pub mod blk {
    pub const NAME: usize = 0;
    pub const PATCH_0: usize = 1;
    pub const FX1: usize = 3;
    pub const FX2: usize = 4;
    pub const DELAY1: usize = 5;
    pub const DELAY2: usize = 6;
    pub const PATCH_1: usize = 7;
    pub const STATUS: usize = 9;
}

/// LED colour of an effect section on the amp panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Led {
    Off,
    Green,
    Red,
    Yellow,
}

#[derive(Debug, Clone)]
pub struct Summary {
    pub name: String,
    pub amp: String,
    pub gain: i16,
    pub volume: i16,
    /// booster, mod, fx, delay, reverb
    pub leds: [Led; 5],
    pub fx_names: [String; 5],
}

impl Default for Patch {
    fn default() -> Self {
        let blocks = PATCH_BLOCKS
            .iter()
            .map(|b| {
                let mut v = vec![0u8; b.len];
                for p in b.params {
                    match p.kind {
                        crate::map::ParamKind::Text => {
                            v[..16].copy_from_slice(b"KATANA Mk2      ");
                        }
                        _ => {
                            let enc = p.encode(p.init);
                            v[p.off as usize..p.off as usize + enc.len()].copy_from_slice(&enc);
                        }
                    }
                }
                v
            })
            .collect();
        Patch { blocks }
    }
}

impl Patch {
    pub fn block(&self, i: usize) -> &[u8] {
        &self.blocks[i]
    }

    /// Replace a block's bytes (length-checked; shorter input is zero-padded).
    pub fn set_block(&mut self, i: usize, data: &[u8]) {
        let b = &mut self.blocks[i];
        let n = data.len().min(b.len());
        b[..n].copy_from_slice(&data[..n]);
    }

    /// Write bytes at a linear address relative to the patch base (as reported by the amp).
    /// Returns true if any block was touched.
    pub fn apply_rel(&mut self, rel: u32, data: &[u8]) -> bool {
        let mut hit = false;
        for (i, b) in PATCH_BLOCKS.iter().enumerate() {
            let start = b.addr;
            let end = start + b.len as u32;
            let (ds, de) = (rel, rel + data.len() as u32);
            let (s, e) = (ds.max(start), de.min(end));
            if s < e {
                let dst = &mut self.blocks[i][(s - start) as usize..(e - start) as usize];
                dst.copy_from_slice(&data[(s - ds) as usize..(e - ds) as usize]);
                hit = true;
            }
        }
        hit
    }

    pub fn name(&self) -> String {
        String::from_utf8_lossy(&self.blocks[blk::NAME]).trim_end().to_string()
    }

    pub fn set_name(&mut self, name: &str) {
        let mut n = [b' '; 16];
        for (i, c) in name.chars().filter(|c| (' '..='}').contains(c)).take(16).enumerate() {
            n[i] = c as u8;
        }
        self.blocks[blk::NAME].copy_from_slice(&n);
    }

    pub fn get(&self, r: ParamRef) -> i16 {
        let o = r.param.off as usize;
        r.param.decode(&self.blocks[r.block][o..o + r.param.width()])
    }

    /// Set a value; returns (relative address, encoded bytes) for sending to the amp.
    pub fn set(&mut self, r: ParamRef, v: i16) -> (u32, Vec<u8>) {
        let enc = r.param.encode(v);
        let o = r.param.off as usize;
        self.blocks[r.block][o..o + enc.len()].copy_from_slice(&enc);
        (r.rel_addr(), enc)
    }

    fn raw(&self, b: usize, off: usize) -> i16 {
        self.blocks[b][off] as i16
    }

    pub fn summary(&self) -> Summary {
        let led = |x: i16| match x {
            1 => Led::Green,
            2 => Led::Red,
            3 => Led::Yellow,
            _ => Led::Off,
        };
        let st = blk::STATUS;
        let on = |b: usize, off: usize| self.raw(b, off) != 0;
        // Status LED state is what the panel shows; mask with the block's real SW.
        let sw = [
            on(blk::PATCH_0, 0),
            on(blk::FX1, 0),
            on(blk::FX2, 0),
            on(blk::DELAY1, 0),
            on(blk::PATCH_1, 0),
        ];
        let mut leds = [Led::Off; 5];
        for i in 0..5 {
            let l = led(self.raw(st, 13 + i));
            leds[i] = if sw[i] { if l == Led::Off { Led::Green } else { l } } else { Led::Off };
        }
        let lab = |b: usize, idx: u16, v: i16| {
            PATCH_BLOCKS[b].params.iter().find(|p| p.idx == idx).and_then(|p| p.label(v)).unwrap_or("").to_string()
        };
        let fx_names = [
            lab(blk::PATCH_0, 1, self.raw(blk::PATCH_0, 1)),
            lab(blk::FX1, 1, self.raw(blk::FX1, 1)),
            lab(blk::FX2, 1, self.raw(blk::FX2, 1)),
            lab(blk::DELAY1, 1, self.raw(blk::DELAY1, 1)),
            lab(blk::PATCH_1, 1, self.raw(blk::PATCH_1, 1)),
        ];
        Summary {
            name: self.name(),
            amp: amp_name(self.raw(blk::PATCH_0, 0x11)),
            gain: self.raw(blk::PATCH_0, 0x12),
            volume: self.raw(blk::PATCH_0, 0x18),
            leds,
            fx_names,
        }
    }

    /// Stable content id (FNV-1a 64 over all blocks).
    pub fn content_hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in &self.blocks {
            for &x in b {
                h ^= x as u64;
                h = h.wrapping_mul(0x100_0000_01b3);
            }
        }
        h
    }

    /// DT1 messages that load this patch into an area (`base` = TEMP_PATCH or user_patch(n)).
    /// `with_status`: BTS skips the Status block for the temporary area.
    pub fn dt1_messages(&self, dev: u8, base: u32, with_status: bool) -> Vec<Vec<u8>> {
        PATCH_BLOCKS
            .iter()
            .enumerate()
            .filter(|(i, _)| with_status || *i != blk::STATUS)
            .map(|(i, b)| sysex::dt1(dev, base + b.addr, &self.blocks[i]))
            .collect()
    }

    /// Iterate (block key, bytes).
    pub fn iter_blocks(&self) -> impl Iterator<Item = (&'static str, &[u8])> {
        PATCH_BLOCKS.iter().zip(self.blocks.iter()).map(|(d, b)| (d.key, b.as_slice()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::Key;

    #[test]
    fn default_and_set() {
        let mut p = Patch::default();
        assert_eq!(p.name(), "KATANA Mk2");
        let gain = ParamRef::find("Patch_0", Key::Prm("PRM_PREAMP_A_GAIN")).unwrap();
        assert_eq!(p.get(gain), 60);
        let (addr, bytes) = p.set(gain, 99);
        assert_eq!(addr, crate::sysex::lin(0x22));
        assert_eq!(bytes, vec![99]);
        let dt = ParamRef::find("Delay(1)", Key::Prm("PRM_DLY_COMMON_DLY_TIME")).unwrap();
        p.set(dt, 1500);
        assert_eq!(p.get(dt), 1500);
    }

    #[test]
    fn apply_rel_spans_blocks() {
        let mut p = Patch::default();
        assert!(p.apply_rel(0, b"Hello"));
        assert_eq!(p.name(), "HelloA Mk2");
    }
}
