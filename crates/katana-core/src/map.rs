//! Static parameter map (generated from BOSS TONE STUDIO) plus hand-written semantics.

pub use crate::map_gen::PATCH_BLOCKS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    /// one byte, 0..=127
    I7,
    /// two 7-bit bytes, big endian (delay times etc.)
    I14,
    /// 16 ASCII bytes
    Text,
}

#[derive(Debug)]
pub struct ParamDef {
    pub name: &'static str,
    /// index inside the BTS array (incl. padding) — stable id used by BTS item ids
    pub idx: u16,
    /// linear byte offset inside the block
    pub off: u16,
    pub kind: ParamKind,
    /// raw = value + ofs
    pub ofs: i16,
    pub init: i16,
    pub prm: &'static str,
    pub min: i16,
    pub max: i16,
    /// (value, label) for enumerations, sorted by value
    pub labels: &'static [(i16, &'static str)],
}

impl ParamDef {
    pub fn width(&self) -> usize {
        match self.kind {
            ParamKind::I7 => 1,
            ParamKind::I14 => 2,
            ParamKind::Text => 16,
        }
    }
    pub fn label(&self, v: i16) -> Option<&'static str> {
        self.labels.iter().find(|(k, _)| *k == v).map(|(_, l)| *l)
    }
    pub fn encode(&self, v: i16) -> Vec<u8> {
        let raw = (v.clamp(self.min, self.max) + self.ofs) as u16;
        match self.kind {
            ParamKind::I14 => vec![((raw >> 7) & 0x7f) as u8, (raw & 0x7f) as u8],
            _ => vec![(raw & 0x7f) as u8],
        }
    }
    pub fn decode(&self, b: &[u8]) -> i16 {
        let raw = match self.kind {
            ParamKind::I14 => ((b[0] as i16) << 7) | b[1] as i16,
            _ => b[0] as i16,
        };
        raw - self.ofs
    }
}

#[derive(Debug)]
pub struct BlockDef {
    pub key: &'static str,
    /// linear offset inside the patch area
    pub addr: u32,
    pub len: usize,
    pub params: &'static [ParamDef],
}

pub fn block_index(key: &str) -> Option<usize> {
    PATCH_BLOCKS.iter().position(|b| b.key == key)
}

/// Address of a parameter: (block index, param) — resolved once at startup by callers.
#[derive(Debug, Clone, Copy)]
pub struct ParamRef {
    pub block: usize,
    pub param: &'static ParamDef,
}

impl ParamRef {
    pub fn find(block: &str, prm_or_idx: Key) -> Option<ParamRef> {
        let b = block_index(block)?;
        let p = PATCH_BLOCKS[b].params.iter().find(|p| match prm_or_idx {
            Key::Prm(s) => p.prm == s,
            Key::Idx(i) => p.idx == i,
        })?;
        Some(ParamRef { block: b, param: p })
    }
    /// Linear address relative to a patch area base.
    pub fn rel_addr(&self) -> u32 {
        PATCH_BLOCKS[self.block].addr + self.param.off as u32
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Key {
    Prm(&'static str),
    Idx(u16),
}

// ---------------------------------------------------------------------------
// Semantics that BTS draws with images instead of data.

/// Panel amp types: (label, normal preamp value, variation preamp value).
pub const AMP_PANEL: [(&str, i16, i16); 5] =
    [("ACOUSTIC", 1, 28), ("CLEAN", 8, 29), ("CRUNCH", 11, 30), ("LEAD", 24, 31), ("BROWN", 23, 32)];

/// Display name of a preamp TYPE value (0..=32). Panel types verified against
/// real patches; the "hidden" models follow the BOSS GT/KATANA preamp list.
pub fn amp_name(v: i16) -> String {
    for (i, (n, a, b)) in AMP_PANEL.iter().enumerate() {
        let _ = i;
        if v == *a {
            return (*n).to_string();
        }
        if v == *b {
            return format!("{n} VARIATION");
        }
    }
    const HIDDEN: [&str; 26] = [
        "NATURAL CLEAN", "ACOUSTIC", "COMBO CRUNCH", "STACK CRUNCH", "HiGAIN STACK", "POWER DRIVE",
        "EXTREME LEAD", "CORE METAL", "CLEAN", "CLEAN TWIN", "PRO CRUNCH", "CRUNCH", "DELUXE CRUNCH",
        "VO DRIVE", "VO LEAD", "MATCH DRIVE", "BG LEAD", "BG DRIVE", "MS1959 I", "MS1959 I+II",
        "R-FIER VINTAGE", "R-FIER MODERN", "T-AMP LEAD", "BROWN", "LEAD", "CUSTOM",
    ];
    HIDDEN.get(v as usize).map(|s| s.to_string()).unwrap_or_else(|| format!("AMP {v}"))
}

/// Which panel family (0..5) a preamp value belongs to, and whether it is the variation.
pub fn amp_family(v: i16) -> Option<(usize, bool)> {
    AMP_PANEL.iter().enumerate().find_map(|(i, (_, a, b))| {
        if v == *a {
            Some((i, false))
        } else if v == *b {
            Some((i, true))
        } else {
            None
        }
    })
}

/// FX-block parameter groups (from BTS `PRM_FX1_<GROUP>_…` ids) → FX TYPE values using them.
pub fn fx_group_types(prm: &str) -> Option<&'static [i16]> {
    let g = prm.strip_prefix("PRM_FX1_")?.split('_').next()?;
    Some(match g {
        "TWAH" => &[0],
        "AWAH" => &[1],
        "SUBWAH" => &[2],
        "ADCOMP" => &[3],
        "LIMITER" => &[4],
        "GEQ" => &[6],
        "PEQ" => &[7],
        "GTRSIM" => &[9],
        "SLOWGEAR" => &[10],
        "WAVESYN" => &[12],
        "OCTAVE" => &[14],
        "PITCHSHIFT" => &[15],
        "HARMONIST" => &[16],
        "ACPROCESS" => &[18],
        "PHASER" => &[19],
        "FLANGER" => &[20],
        "TREMOLO" => &[21],
        "ROTARY" => &[22],
        "UNI" => &[23],
        "SLICER" => &[25],
        "VIBRATO" => &[26],
        "RINGMOD" => &[27],
        "HUMANIZER" => &[28],
        "2" | "2x2CHORUS" => &[29],
        "ACSIM" => &[31],
        "EVH" => &[35, 36, 37],
        "DC30" => &[38],
        "HEAVY" => &[39],
        "PEDAL" => &[40],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocks_match_tsl_sizes() {
        let sizes: Vec<_> = PATCH_BLOCKS.iter().map(|b| (b.key, b.len)).collect();
        assert!(sizes.contains(&("Fx(1)", 225)));
        assert!(sizes.contains(&("Patch_0", 72)));
        assert_eq!(PATCH_BLOCKS.len(), 31);
    }
    #[test]
    fn param_lookup() {
        let t = ParamRef::find("Patch_0", Key::Prm("PRM_PREAMP_A_TYPE")).unwrap();
        assert_eq!(t.rel_addr(), crate::sysex::lin(0x0000_0021)); // 0x10 + 0x11
        let mt = ParamRef::find("Fx(1)", Key::Idx(1)).unwrap();
        assert_eq!(mt.param.label(29), Some("CHORUS"));
        let bt = ParamRef::find("Patch_0", Key::Idx(1)).unwrap();
        assert_eq!(bt.param.label(12), Some("T-SCREAM"));
    }
}
