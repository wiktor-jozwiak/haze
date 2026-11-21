//! Fretboard view of the tuner page (NECK mode): which string / fret positions to mark for a
//! key and scale, and where the note being played sits on the neck.
//!
//! Strings come from the tuner's tuning (`haze_dsp::tuner::TUNINGS`), lowest first, so a
//! 7-string or a bass tuning draws its own neck. Pure functions, no UI types: app.rs turns
//! the positions into `NeckDot` rows.

/// Frets drawn after the nut. Must match `frets` in ui/neck.slint.
pub const FRETS: i32 = 22;

pub struct Scale {
    pub name: &'static str,
    /// semitones above the root
    pub steps: &'static [i32],
}

pub const SCALES: [Scale; 10] = [
    Scale { name: "Minor pentatonic", steps: &[0, 3, 5, 7, 10] },
    Scale { name: "Major pentatonic", steps: &[0, 2, 4, 7, 9] },
    Scale { name: "Blues", steps: &[0, 3, 5, 6, 7, 10] },
    Scale { name: "Natural minor", steps: &[0, 2, 3, 5, 7, 8, 10] },
    Scale { name: "Major", steps: &[0, 2, 4, 5, 7, 9, 11] },
    Scale { name: "Dorian", steps: &[0, 2, 3, 5, 7, 9, 10] },
    Scale { name: "Mixolydian", steps: &[0, 2, 4, 5, 7, 9, 10] },
    Scale { name: "Phrygian", steps: &[0, 1, 3, 5, 7, 8, 10] },
    Scale { name: "Harmonic minor", steps: &[0, 2, 3, 5, 7, 8, 11] },
    Scale { name: "Notes only", steps: &[] },
];

/// One marked position: string index (0 = lowest), fret (0 = open), and the scale degree
/// (0 = root), or `None` when the note is outside the scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pos {
    pub string: usize,
    pub fret: i32,
    pub degree: Option<usize>,
    pub midi: i32,
}

/// Every position of the scale `scale` in `key` (pitch class, 0 = C) on `strings`.
pub fn scale_positions(strings: &[i32], key: i32, scale: &Scale) -> Vec<Pos> {
    let mut out = vec![];
    for (si, &open) in strings.iter().enumerate() {
        for fret in 0..=FRETS {
            let midi = open + fret;
            let pc = (midi - key).rem_euclid(12);
            if let Some(d) = scale.steps.iter().position(|&s| s == pc) {
                out.push(Pos { string: si, fret, degree: Some(d), midi });
            }
        }
    }
    out
}

/// Every place on the neck where exactly `midi` (octave included) can be played.
pub fn note_positions(strings: &[i32], key: i32, scale: &Scale, midi: i32) -> Vec<Pos> {
    let pc = (midi - key).rem_euclid(12);
    let degree = scale.steps.iter().position(|&s| s == pc);
    strings
        .iter()
        .enumerate()
        .filter_map(|(si, &open)| {
            let fret = midi - open;
            (0..=FRETS).contains(&fret).then_some(Pos { string: si, fret, degree, midi })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use haze_dsp::tuner::TUNINGS;

    const STD: &[i32] = TUNINGS[0].strings;

    #[test]
    fn a_minor_pentatonic_box_one() {
        // the classic box at the 5th fret: low E string has A (5) and C (8)
        let p = scale_positions(STD, 9, &SCALES[0]);
        let low_e: Vec<i32> = p.iter().filter(|x| x.string == 0 && (5..=8).contains(&x.fret)).map(|x| x.fret).collect();
        assert_eq!(low_e, vec![5, 8]);
        // roots are A on every string
        assert!(p.iter().filter(|x| x.degree == Some(0)).all(|x| x.midi.rem_euclid(12) == 9));
        // 5 notes per octave, 23 frets per string: 9 or 10 per string
        for s in 0..6 {
            let n = p.iter().filter(|x| x.string == s).count();
            assert!((9..=10).contains(&n), "string {s}: {n}");
        }
    }

    #[test]
    fn a2_on_standard_e() {
        // A2 = MIDI 45: low E fret 5, open A; nothing on higher strings
        let p = note_positions(STD, 9, &SCALES[0], 45);
        let got: Vec<(usize, i32)> = p.iter().map(|x| (x.string, x.fret)).collect();
        assert_eq!(got, vec![(0, 5), (1, 0)]);
        assert!(p.iter().all(|x| x.degree == Some(0)));
    }

    #[test]
    fn out_of_scale_and_out_of_range() {
        // A#3 is not in A minor pentatonic
        let p = note_positions(STD, 9, &SCALES[0], 58);
        assert!(!p.is_empty() && p.iter().all(|x| x.degree.is_none()));
        // below the low E and above the 22nd fret of the high E: nowhere
        assert!(note_positions(STD, 9, &SCALES[0], 39).is_empty());
        assert!(note_positions(STD, 9, &SCALES[0], 64 + FRETS + 1).is_empty());
    }

    #[test]
    fn notes_only_marks_nothing() {
        assert!(scale_positions(STD, 0, &SCALES[9]).is_empty());
    }
}
