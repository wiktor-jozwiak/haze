//! EQ response drawn in the inspector. An illustration of what the knobs do (shelf corners and
//! peak widths are approximations, not BOSS's exact filters), on a log axis 20 Hz – 20 kHz.
//!
//! Path coordinates: x 0..1000 across the band, y 0..200 with 0 dB at y = 100 and ±24 dB at the edges.

/// Parametric EQ settings in engineering units.
#[derive(Clone, Copy, Debug, Default)]
pub struct Para {
    pub low_cut: Option<f32>,
    pub low_gain: f32,
    /// (freq Hz, Q, gain dB)
    pub low_mid: (f32, f32, f32),
    pub high_mid: (f32, f32, f32),
    pub high_gain: f32,
    pub high_cut: Option<f32>,
    pub level: f32,
}

/// Katana index tables are ISO 1/3-octave steps.
pub fn freq_index(i: i16) -> f32 {
    20.0 * 2f32.powf(i as f32 / 3.0)
}
pub fn low_cut_index(i: i16) -> Option<f32> {
    (i > 0).then(|| freq_index(i - 1))
}
pub fn high_cut_index(i: i16) -> Option<f32> {
    (i < 14).then(|| 630.0 * 2f32.powf(i as f32 / 3.0))
}
pub fn q_index(i: i16) -> f32 {
    0.5 * 2f32.powi(i as i32)
}

const LOW_SHELF: f32 = 120.0;
const HIGH_SHELF: f32 = 6000.0;

fn peak(f: f32, (f0, q, g): (f32, f32, f32)) -> f32 {
    let w = 0.6 / q.max(0.1); // width in octaves
    let o = (f / f0).log2() / w;
    g * (-0.5 * o * o).exp()
}

pub fn para_db(p: &Para, f: f32) -> f32 {
    let mut db = p.level;
    db += p.low_gain / (1.0 + (f / LOW_SHELF).powi(2));
    db += p.high_gain / (1.0 + (HIGH_SHELF / f).powi(2));
    db += peak(f, p.low_mid) + peak(f, p.high_mid);
    if let Some(fc) = p.low_cut {
        db -= 10.0 * (1.0 + (fc / f).powi(4)).log10();
    }
    if let Some(fc) = p.high_cut {
        db -= 10.0 * (1.0 + (f / fc).powi(4)).log10();
    }
    db
}

/// GE-10: bands at 31.25 Hz · 2^i, smooth (cosine) interpolation on the log axis.
pub fn geq_db(bands: &[f32; 10], level: f32, f: f32) -> f32 {
    let x = (f / 31.25).log2().clamp(0.0, 9.0);
    let i = (x.floor() as usize).min(8);
    let t = x - i as f32;
    let s = (1.0 - (t * std::f32::consts::PI).cos()) / 2.0;
    level + bands[i] * (1.0 - s) + bands[i + 1] * s
}

const N: usize = 160;

/// (curve, filled area) path commands for a response function.
pub fn paths(db_at: impl Fn(f32) -> f32) -> (String, String) {
    let mut c = String::with_capacity(N * 16);
    for k in 0..=N {
        let x = k as f32 / N as f32;
        let f = 20.0 * 1000f32.powf(x);
        let y = (100.0 - db_at(f).clamp(-24.0, 24.0) * 100.0 / 24.0).clamp(0.0, 200.0);
        c.push_str(&format!("{}{:.1} {:.1} ", if k == 0 { "M" } else { "L" }, x * 1000.0, y));
    }
    let fill = format!("{c}L1000 100 L0 100 Z");
    (c.trim_end().to_string(), fill)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_match_display_steps() {
        // ISO 1/3-octave labels are nominal: exact steps sit within ~2 % of them
        let near = |a: f32, b: f32| (a / b - 1.0).abs() < 0.03;
        assert!(near(freq_index(0), 20.0));
        assert!(near(freq_index(17), 1000.0)); // "1 kHz"
        assert!(near(freq_index(27), 10000.0)); // "10 kHz"
        assert_eq!(low_cut_index(0), None);
        assert!(near(low_cut_index(8).unwrap(), 100.0)); // "100 Hz"
        assert_eq!(high_cut_index(14), None);
        assert!(near(high_cut_index(12).unwrap(), 10000.0)); // "10 kHz"
        assert_eq!(q_index(1), 1.0);
    }

    #[test]
    fn flat_is_flat_and_boost_shows() {
        let p = Para { low_mid: (500.0, 1.0, 0.0), high_mid: (2000.0, 1.0, 0.0), ..Default::default() };
        for f in [20.0, 100.0, 1000.0, 10000.0] {
            assert!(para_db(&p, f).abs() < 1e-3);
        }
        let p = Para { low_mid: (500.0, 1.0, 12.0), ..p };
        assert!((para_db(&p, 500.0) - 12.0).abs() < 0.01);
        assert!(para_db(&p, 5000.0) < 1.0);
        let cut = Para { low_cut: Some(200.0), ..Default::default() };
        assert!(para_db(&cut, 50.0) < -20.0);
        assert!(para_db(&cut, 2000.0) > -0.1);
        let mut b = [0.0; 10];
        b[5] = 10.0; // 1 kHz
        assert!((geq_db(&b, 0.0, 1000.0) - 10.0).abs() < 0.01);
        assert!(geq_db(&b, 0.0, 125.0).abs() < 0.01);
        let (c, fill) = paths(|f| geq_db(&b, 0.0, f));
        assert!(c.starts_with("M0.0 100.0") && fill.ends_with('Z'));
    }
}
