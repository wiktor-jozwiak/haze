//! On-disk tone library.
//!
//! ```text
//! $XDG_DATA_HOME/katana-studio/
//!   tones/<id>.json      one tone per file (blocks as hex) — diff/git/sync friendly
//!   setlists/<id>.json   ordered lists of tone ids (imported BTS livesets, amp backups, own sets)
//!   trash/               deleted tones (never hard-deleted by the app)
//! ```
//! Tones are deduplicated by content hash on import, so the same patch living in
//! five BTS livesets becomes one tone referenced by five setlists.

use crate::map::block_index;
use crate::patch::Patch;
use crate::tsl::LiveSet;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tone {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub favorite: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub memo: String,
    /// where it came from: "BTS · Master of Puppets", "AMP · A2", "nowy"
    #[serde(default)]
    pub origin: String,
    pub created: u64,
    pub modified: u64,
    /// block key → hex bytes
    pub blocks: BTreeMap<String, String>,
    #[serde(skip)]
    pub patch: Patch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SetlistKind {
    #[default]
    Setlist,
    Backup,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Setlist {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: SetlistKind,
    pub created: u64,
    pub tones: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub struct ImportReport {
    pub setlists: usize,
    pub patches: usize,
    pub new_tones: usize,
    pub duplicates: usize,
    pub skipped_setlists: usize,
}

pub struct Library {
    root: PathBuf,
    pub tones: Vec<Tone>,
    pub setlists: Vec<Setlist>,
    by_hash: HashMap<u64, usize>,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).filter_map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()).collect()
}

fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data)?;
    fs::rename(tmp, path)
}

fn new_id(seed: u64) -> String {
    // time-ordered, collision-safe enough for a personal library
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) as u64;
    format!("{:010x}{:06x}", t >> 20, (seed ^ t) & 0xff_ffff)
}

impl Tone {
    fn sync_blocks(&mut self) {
        self.blocks = self.patch.iter_blocks().map(|(k, b)| (k.to_string(), hex(b))).collect();
    }
    fn load_patch(&mut self) {
        let mut p = Patch::default();
        for (k, v) in &self.blocks {
            if let Some(i) = block_index(k) {
                p.set_block(i, &unhex(v));
            }
        }
        self.patch = p;
    }
}

/// `$HOME`, or `%USERPROFILE%` on Windows (where HOME is usually unset).
pub fn home_dir() -> PathBuf {
    let v = std::env::var_os("HOME").filter(|h| !h.is_empty());
    #[cfg(windows)]
    let v = v.or_else(|| std::env::var_os("USERPROFILE"));
    PathBuf::from(v.unwrap_or_default())
}

impl Library {
    /// `$XDG_DATA_HOME/katana-studio` (`~/.local/share/…`); `%APPDATA%\katana-studio` on Windows.
    pub fn default_root() -> PathBuf {
        #[cfg(windows)]
        if let Some(d) = std::env::var_os("APPDATA") {
            return PathBuf::from(d).join("katana-studio");
        }
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".local/share"));
        base.join("katana-studio")
    }

    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        for d in ["tones", "setlists", "trash"] {
            fs::create_dir_all(root.join(d))?;
        }
        let mut lib = Library { root, tones: vec![], setlists: vec![], by_hash: HashMap::new() };
        for e in fs::read_dir(lib.root.join("tones"))?.flatten() {
            if e.path().extension().is_some_and(|x| x == "json") {
                if let Ok(mut t) = serde_json::from_slice::<Tone>(&fs::read(e.path())?) {
                    t.load_patch();
                    lib.tones.push(t);
                }
            }
        }
        for e in fs::read_dir(lib.root.join("setlists"))?.flatten() {
            if e.path().extension().is_some_and(|x| x == "json") {
                if let Ok(s) = serde_json::from_slice::<Setlist>(&fs::read(e.path())?) {
                    lib.setlists.push(s);
                }
            }
        }
        lib.tones.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        lib.setlists.sort_by_key(|s| (s.kind == SetlistKind::Backup, s.created));
        lib.reindex();
        Ok(lib)
    }

    fn reindex(&mut self) {
        self.by_hash = self.tones.iter().enumerate().map(|(i, t)| (t.patch.content_hash(), i)).collect();
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn tone(&self, id: &str) -> Option<&Tone> {
        self.tones.iter().find(|t| t.id == id)
    }

    pub fn tone_mut(&mut self, id: &str) -> Option<&mut Tone> {
        self.tones.iter_mut().find(|t| t.id == id)
    }

    pub fn find_by_content(&self, p: &Patch) -> Option<&Tone> {
        self.by_hash.get(&p.content_hash()).map(|&i| &self.tones[i])
    }

    fn write_tone(&self, t: &Tone) -> io::Result<()> {
        atomic_write(&self.root.join("tones").join(format!("{}.json", t.id)), &serde_json::to_vec_pretty(t)?)
    }

    fn write_setlist(&self, s: &Setlist) -> io::Result<()> {
        atomic_write(&self.root.join("setlists").join(format!("{}.json", s.id)), &serde_json::to_vec_pretty(s)?)
    }

    /// Add a patch; returns (tone id, was_new). Identical content returns the existing tone.
    pub fn add_patch(&mut self, patch: Patch, origin: &str, memo: &str) -> io::Result<(String, bool)> {
        if let Some(t) = self.find_by_content(&patch) {
            return Ok((t.id.clone(), false));
        }
        let ts = now();
        let mut t = Tone {
            id: new_id(patch.content_hash()),
            name: patch.name(),
            favorite: false,
            tags: vec![],
            memo: memo.to_string(),
            origin: origin.to_string(),
            created: ts,
            modified: ts,
            blocks: BTreeMap::new(),
            patch,
        };
        t.sync_blocks();
        self.write_tone(&t)?;
        let id = t.id.clone();
        self.by_hash.insert(t.patch.content_hash(), self.tones.len());
        self.tones.push(t);
        Ok((id, true))
    }

    /// Persist changes to a tone (patch, name, favorite, tags…).
    pub fn update_tone(&mut self, id: &str, f: impl FnOnce(&mut Tone)) -> io::Result<()> {
        let Some(t) = self.tone_mut(id) else { return Err(io::Error::new(io::ErrorKind::NotFound, id.to_string())) };
        f(t);
        if t.patch.name() != t.name {
            let n = t.name.clone();
            t.patch.set_name(&n);
            t.name = t.patch.name();
        }
        t.modified = now();
        t.sync_blocks();
        let t = t.clone();
        self.write_tone(&t)?;
        self.reindex();
        Ok(())
    }

    /// Move a tone to trash/ and drop it from setlists.
    pub fn trash_tone(&mut self, id: &str) -> io::Result<()> {
        let from = self.root.join("tones").join(format!("{id}.json"));
        let to = self.root.join("trash").join(format!("{id}.json"));
        if from.exists() {
            fs::rename(from, to)?;
        }
        self.tones.retain(|t| t.id != id);
        let mut touched = vec![];
        for s in &mut self.setlists {
            let n = s.tones.len();
            s.tones.retain(|t| t != id);
            if s.tones.len() != n {
                touched.push(s.clone());
            }
        }
        for s in touched {
            self.write_setlist(&s)?;
        }
        self.reindex();
        Ok(())
    }

    pub fn add_setlist(&mut self, name: &str, kind: SetlistKind, tones: Vec<String>) -> io::Result<String> {
        let s = Setlist { id: new_id(tones.len() as u64), name: name.to_string(), kind, created: now(), tones };
        self.write_setlist(&s)?;
        let id = s.id.clone();
        self.setlists.push(s);
        Ok(id)
    }

    pub fn update_setlist(&mut self, id: &str, f: impl FnOnce(&mut Setlist)) -> io::Result<()> {
        let Some(s) = self.setlists.iter_mut().find(|s| s.id == id) else { return Ok(()) };
        f(s);
        let s = s.clone();
        self.write_setlist(&s)
    }

    pub fn import_liveset(&mut self, ls: LiveSet, origin: &str, rep: &mut ImportReport) -> io::Result<()> {
        if ls.patches.is_empty() {
            rep.skipped_setlists += 1;
            return Ok(());
        }
        let mut ids = vec![];
        for (p, memo) in ls.patches {
            rep.patches += 1;
            let (id, new) = self.add_patch(p, origin, &memo)?;
            if new {
                rep.new_tones += 1;
            } else {
                rep.duplicates += 1;
            }
            ids.push(id);
        }
        // re-import of the same liveset is idempotent
        if !self.setlists.iter().any(|s| s.name == ls.name && s.tones == ids) {
            self.add_setlist(&ls.name, SetlistKind::Setlist, ids)?;
            rep.setlists += 1;
        }
        Ok(())
    }

    /// Import BTS' internal library dir (`…/BOSS TONE STUDIO for KATANA MkII/pref`), in BTS order.
    pub fn import_bts_pref_dir(&mut self, dir: &Path) -> io::Result<ImportReport> {
        let mut order: Vec<String> = fs::read_to_string(dir.join("ID_LIST"))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("list").and_then(|l| l.as_array()).cloned())
            .map(|l| l.iter().map(|x| x.to_string()).collect())
            .unwrap_or_default();
        order.reverse(); // ID_LIST is newest-first; import oldest first so setlist order matches BTS
        if order.is_empty() {
            order = fs::read_dir(dir)?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            order.sort();
        }
        let mut rep = ImportReport::default();
        for f in order {
            let Ok(text) = fs::read_to_string(dir.join(&f)) else { continue };
            let Ok(ls) = crate::tsl::parse(&text) else { continue };
            let origin = format!("BTS · {}", ls.name);
            self.import_liveset(ls, &origin, &mut rep)?;
        }
        Ok(rep)
    }

    pub fn import_tsl_file(&mut self, path: &Path) -> io::Result<ImportReport> {
        let text = fs::read_to_string(path)?;
        let ls = crate::tsl::parse(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut rep = ImportReport::default();
        let origin = format!("TSL · {}", ls.name);
        self.import_liveset(ls, &origin, &mut rep)?;
        Ok(rep)
    }

    pub fn export_tsl(&self, name: &str, ids: &[String], path: &Path) -> io::Result<()> {
        let ts: Vec<_> = ids.iter().filter_map(|i| self.tone(i)).map(|t| (&t.patch, t.memo.as_str())).collect();
        atomic_write(path, crate::tsl::write(name, &ts).as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupe_and_persist() {
        let dir = std::env::temp_dir().join(format!("katana-lib-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut lib = Library::open(&dir).unwrap();
        let mut p = Patch::default();
        p.set_name("Crunchy");
        let (a, new_a) = lib.add_patch(p.clone(), "t", "").unwrap();
        let (b, new_b) = lib.add_patch(p.clone(), "t", "").unwrap();
        assert!(new_a && !new_b && a == b);
        lib.update_tone(&a, |t| {
            t.name = "Renamed".into();
            t.favorite = true;
        })
        .unwrap();
        let lib2 = Library::open(&dir).unwrap();
        let t = lib2.tone(&a).unwrap();
        assert_eq!(t.patch.name(), "Renamed");
        assert!(t.favorite);
        fs::remove_dir_all(&dir).unwrap();
    }
}
