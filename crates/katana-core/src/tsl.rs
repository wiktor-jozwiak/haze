//! BOSS TONE STUDIO liveset (`.tsl`) codec.
//!
//! Format: `{"name","formatRev","device","data":[[{"memo","paramSet":{"UserPatch%<Block>":["HH",…]}}]]}`.
//! BTS' internal library (`AppData/Local/Roland/…/pref/<n>`) stores the same JSON, string-encoded once more.

use crate::map::{block_index, PATCH_BLOCKS};
use crate::patch::Patch;
use serde_json::{json, Map, Value};

#[derive(Debug, Clone)]
pub struct LiveSet {
    pub name: String,
    pub device: String,
    pub patches: Vec<(Patch, String)>, // (patch, memo)
}

#[derive(Debug)]
pub struct TslError(pub String);
impl std::fmt::Display for TslError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for TslError {}

fn err<T>(s: impl Into<String>) -> Result<T, TslError> {
    Err(TslError(s.into()))
}

/// Parse `.tsl` text, or BTS' double-encoded pref file.
pub fn parse(text: &str) -> Result<LiveSet, TslError> {
    let text = text.trim_start_matches('\u{feff}');
    let mut v: Value = serde_json::from_str(text).map_err(|e| TslError(e.to_string()))?;
    if let Value::String(inner) = &v {
        v = serde_json::from_str(inner).map_err(|e| TslError(e.to_string()))?;
    }
    let name = v.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    let device = v.get("device").and_then(Value::as_str).unwrap_or("").to_string();
    let Some(groups) = v.get("data").and_then(Value::as_array) else { return err("missing data") };
    let mut patches = Vec::new();
    for g in groups {
        for cell in g.as_array().into_iter().flatten() {
            let Some(ps) = cell.get("paramSet").and_then(Value::as_object) else { continue };
            let mut p = Patch::default();
            for (k, arr) in ps {
                let Some(key) = k.split('%').nth(1) else { continue };
                let Some(bi) = block_index(key) else { continue };
                let bytes: Vec<u8> = arr
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|h| h.as_str().and_then(|s| u8::from_str_radix(s, 16).ok()))
                    .collect();
                p.set_block(bi, &bytes);
            }
            let memo = match cell.get("memo") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Object(o)) => o.get("memo").and_then(Value::as_str).unwrap_or("").to_string(),
                _ => String::new(),
            };
            patches.push((p, memo));
        }
    }
    Ok(LiveSet { name, device, patches })
}

/// Serialize as a `.tsl` file BTS for KATANA MkII can import.
pub fn write(name: &str, patches: &[(&Patch, &str)]) -> String {
    let cells: Vec<Value> = patches
        .iter()
        .map(|(p, memo)| {
            let mut ps = Map::new();
            for (key, bytes) in p.iter_blocks() {
                ps.insert(
                    format!("UserPatch%{key}"),
                    Value::Array(bytes.iter().map(|b| Value::String(format!("{b:02X}"))).collect()),
                );
            }
            json!({ "memo": memo, "paramSet": ps })
        })
        .collect();
    let v = json!({ "name": name, "formatRev": "0002", "device": "KATANA MkII", "data": [cells] });
    serde_json::to_string(&v).unwrap()
}

#[allow(dead_code)]
fn _assert_blocks() {
    let _ = PATCH_BLOCKS;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let mut p = Patch::default();
        p.set_name("Test Tone");
        let s = write("Set", &[(&p, "memo")]);
        let ls = parse(&s).unwrap();
        assert_eq!(ls.name, "Set");
        assert_eq!(ls.patches[0].0, p);
        assert_eq!(ls.patches[0].1, "memo");
        // double-encoded (BTS pref) form
        let dbl = serde_json::to_string(&Value::String(s)).unwrap();
        assert_eq!(parse(&dbl).unwrap().patches[0].0, p);
    }
}
