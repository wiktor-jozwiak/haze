//! Undo/redo for the edited patch: whole-state snapshots, edits closer than `MERGE` apart
//! collapse into one step (a knob drag or a slider sweep undoes in one go).

use std::time::{Duration, Instant};

const MERGE: Duration = Duration::from_millis(400);
const CAP: usize = 200;

pub struct History<T> {
    undo: Vec<T>,
    redo: Vec<T>,
    /// state after the last committed step
    base: T,
    last: Option<Instant>,
}

impl<T: Clone + PartialEq> History<T> {
    pub fn new(base: T) -> Self {
        Self { undo: Vec::new(), redo: Vec::new(), base, last: None }
    }

    /// Forget everything; `base` is the new starting point (another tone was loaded).
    pub fn reset(&mut self, base: T) {
        self.undo.clear();
        self.redo.clear();
        self.base = base;
        self.last = None;
    }

    /// Record `cur` after an edit. No-op when nothing changed.
    pub fn commit(&mut self, cur: &T, now: Instant) {
        if *cur == self.base {
            return;
        }
        let merge = self.last.is_some_and(|t| now.duration_since(t) < MERGE) && !self.undo.is_empty();
        if !merge {
            let prev = std::mem::replace(&mut self.base, cur.clone());
            self.undo.push(prev);
            if self.undo.len() > CAP {
                self.undo.remove(0);
            }
        } else {
            self.base = cur.clone();
        }
        self.redo.clear();
        self.last = Some(now);
    }

    pub fn undo(&mut self, cur: &T) -> Option<T> {
        let u = self.undo.pop()?;
        self.redo.push(cur.clone());
        self.base = u.clone();
        self.last = None;
        Some(u)
    }

    pub fn redo(&mut self, cur: &T) -> Option<T> {
        let r = self.redo.pop()?;
        self.undo.push(cur.clone());
        self.base = r.clone();
        self.last = None;
        Some(r)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_merge_and_redo_clears() {
        let t0 = Instant::now();
        let mut h = History::new(0);
        h.commit(&1, t0);
        h.commit(&2, t0 + Duration::from_millis(100)); // same drag
        h.commit(&3, t0 + Duration::from_secs(2)); // new step
        assert_eq!(h.undo(&3), Some(2));
        assert_eq!(h.undo(&2), Some(0));
        assert_eq!(h.undo(&0), None);
        assert_eq!(h.redo(&0), Some(2));
        // a new edit after undo drops the redo branch
        h.commit(&9, t0 + Duration::from_secs(5));
        assert!(!h.can_redo());
        assert_eq!(h.undo(&9), Some(2));
    }

    #[test]
    fn unchanged_commit_is_noop_and_cap_holds() {
        let t0 = Instant::now();
        let mut h = History::new(0);
        h.commit(&0, t0);
        assert!(!h.can_undo());
        for i in 1..=(CAP as i32 + 50) {
            h.commit(&i, t0 + Duration::from_secs(i as u64));
        }
        let mut n = 0;
        let mut cur = CAP as i32 + 50;
        while let Some(p) = h.undo(&cur) {
            cur = p;
            n += 1;
        }
        assert_eq!(n, CAP);
    }
}
