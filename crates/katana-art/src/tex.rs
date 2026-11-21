//! Seamless background tiles for ui/tex: tolex, brushed gunmetal, pedalboard felt.
//! Low contrast on purpose: they sit under text and controls, so the grain must read as a
//! material, never as noise. Mean brightness is fixed so the palette in theme.slint holds.
//!
//! usage: cargo run -p katana-art --release -- tex crates/katana-studio/ui/tex

use crate::{fbm, hash, mix, norm3, rgb, save, sstep, vnoise};
use std::path::Path;

/// Periodic Worley noise: distances to the nearest and second nearest feature point,
/// in cell units, one jittered point per cell, cells wrap at `n`.
fn worley(x: f32, y: f32, n: i32, seed: u32) -> (f32, f32) {
    let (xi, yi) = (x.floor() as i32, y.floor() as i32);
    let (mut d1, mut d2) = (9f32, 9f32);
    for j in -1..=1 {
        for i in -1..=1 {
            let (cx, cy) = (xi + i, yi + j);
            let (wx, wy) = (cx.rem_euclid(n), cy.rem_euclid(n));
            let px = cx as f32 + 0.05 + 0.9 * hash(wx, wy, seed);
            let py = cy as f32 + 0.05 + 0.9 * hash(wx, wy, seed + 101);
            let d = ((px - x).powi(2) + (py - y).powi(2)).sqrt();
            if d < d1 {
                d2 = d1;
                d1 = d;
            } else if d < d2 {
                d2 = d;
            }
        }
    }
    (d1, d2)
}

/// Levant-grain tolex: small domed pebbles split by thin creases, a faint sheen that drifts
/// over the sheet, lit from the upper left.
fn tolex(dir: &Path) {
    let n = 256usize;
    let cells = 40; // ~6.4 px pebbles at 1x
    let s = cells as f32 / n as f32;
    let height = |x: f32, y: f32| {
        let (d1, d2) = worley(x * s, y * s, cells, 11);
        let dome = 1.0 - sstep(0.0, 0.75, d1);
        let crease = sstep(0.0, 0.12, d2 - d1);
        dome * 0.6 + crease * 0.4
    };
    save(dir, "tolex.png", n, n, |x, y| {
        let e = 0.5;
        let dx = height(x + e, y) - height(x - e, y);
        let dy = height(x, y + e) - height(x, y - e);
        let nrm = norm3([-dx * 1.6, -dy * 1.6, 1.0]);
        let l = norm3([-0.4, -0.7, 0.6]);
        let diff = (nrm[0] * l[0] + nrm[1] * l[1] + nrm[2] * l[2]).max(0.0);
        // broad, very soft sheen variation (vinyl is never perfectly even)
        let sheen = fbm(x / 64.0, y / 64.0, 4, 4, 3, 5);
        let grain = hash(x as i32, y as i32, 3) - 0.5;
        let v = 0.112 * (0.72 + 0.46 * diff) * (0.92 + 0.16 * sheen) + grain * 0.005;
        rgb(v * 1.04, v, v * 0.95)
    });
}

/// Brushed gunmetal: long horizontal streaks of varying length and depth, every row a little
/// different, a fine sparkle. Wide tile so no repeat shows across a panel.
fn brushed(dir: &Path) {
    let (w, h) = (1024usize, 128usize);
    save(dir, "brushed.png", w, h, |x, y| {
        let row = y.floor();
        // streak field: stretched along x (periods divide the tile width), one row per pixel
        let a = vnoise(x / 256.0, row, 4, h as i32, 21) - 0.5;
        let b = vnoise(x / 64.0, row, 16, h as i32, 22) - 0.5;
        let c = vnoise(x / 16.0, row, 64, h as i32, 23) - 0.5;
        // a few deeper scratches
        let deep = (vnoise(x / 128.0, row, 8, h as i32, 24) - 0.5).max(0.0) * 2.0;
        let deep = deep.powi(3) * if hash(0, row as i32, 25) > 0.85 { 1.0 } else { 0.0 };
        let sparkle = hash(x as i32, y as i32, 26) - 0.5;
        // soft vertical swell so the plate does not look printed
        let swell = (fbm(x / 512.0, y / 64.0, 2, 2, 2, 27) - 0.5) * 0.010;
        let v = 0.172 + a * 0.020 + b * 0.014 + c * 0.008 - deep * 0.018 + sparkle * 0.006 + swell;
        rgb(v * 0.97, v * 0.985, v * 1.03)
    });
}

/// Needle felt: fine random fibres over a dark blue-grey, slightly mottled.
fn felt(dir: &Path) {
    let n = 256usize;
    save(dir, "felt.png", n, n, |x, y| {
        let mottle = fbm(x / 32.0, y / 32.0, 8, 8, 3, 31) - 0.5;
        // fibres: short strokes in random directions = anisotropic noise at two angles
        let f1 = vnoise((x + y) / 3.0, (x - y) / 0.9, 128, 256, 32) - 0.5;
        let f2 = vnoise((x - y * 0.5) / 2.5, (y + x * 0.5) / 0.8, 128, 256, 33) - 0.5;
        let fib = mix(f1, f2, 0.5);
        let grain = hash(x as i32, y as i32, 34) - 0.5;
        let v = 0.098 + mottle * 0.012 + fib * 0.014 + grain * 0.006;
        rgb(v * 0.96, v * 0.98, v * 1.08)
    });
}

/// Dot grid for the pedalboard tray: one soft 1.4 px dot per 22 px cell, alpha only.
fn dots(dir: &Path) {
    let n = 22usize;
    save(dir, "dots.png", n, n, |x, y| {
        let (dx, dy) = (x - n as f32 / 2.0, y - n as f32 / 2.0);
        let d = (dx * dx + dy * dy).sqrt();
        let a = (1.0 - sstep(0.4, 1.1, d)) * 0.09;
        [a, a, a, a]
    });
}

pub fn run(dir: &Path) {
    dots(dir);
    tolex(dir);
    brushed(dir);
    felt(dir);
}
