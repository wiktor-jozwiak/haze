//! Procedural renderer for the Katana Studio UI bitmaps.
//!
//! The Slint software renderer cannot rotate or light anything, so all "physical" surfaces are
//! pre-rendered here once, at 1x logical size (no runtime scaling cost):
//! - tiles: tolex, brushed brass, velcro board
//! - sprite sheets: knobs with N rotation frames stacked vertically (lighting fixed, body rotates)
//! - pedal enclosure shading overlay (alpha, drawn over a coloured body)
//! - chrome footswitch, LEDs, jewel lamp, jacks
//!
//! usage: cargo run -p katana-art --release -- <out-dir>

mod tex;

use std::f32::consts::PI;
use std::path::Path;

type C = [f32; 4]; // premultiplied rgba, 0..1

const SS: usize = 4; // supersampling per axis

fn save(dir: &Path, name: &str, w: usize, h: usize, f: impl Fn(f32, f32) -> C) {
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0f32; 4];
            for sy in 0..SS {
                for sx in 0..SS {
                    let c = f(x as f32 + (sx as f32 + 0.5) / SS as f32, y as f32 + (sy as f32 + 0.5) / SS as f32);
                    for i in 0..4 {
                        acc[i] += c[i];
                    }
                }
            }
            let n = (SS * SS) as f32;
            let a = (acc[3] / n).clamp(0.0, 1.0);
            let o = (y * w + x) * 4;
            for i in 0..3 {
                let v = if a > 0.0 { acc[i] / n / a } else { 0.0 };
                out[o + i] = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
            out[o + 3] = (a * 255.0 + 0.5) as u8;
        }
    }
    let file = std::fs::File::create(dir.join(name)).expect("create png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Best);
    enc.write_header().unwrap().write_image_data(&out).unwrap();
    println!("{name}: {w}x{h}");
}

// ---------------------------------------------------------------- math

fn sstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
fn rgb(r: f32, g: f32, b: f32) -> C {
    [r, g, b, 1.0]
}
fn scale(c: C, k: f32) -> C {
    [c[0] * k, c[1] * k, c[2] * k, c[3]]
}
fn add(c: C, k: f32) -> C {
    [c[0] + k * c[3], c[1] + k * c[3], c[2] + k * c[3], c[3]]
}
fn alpha(c: C, a: f32) -> C {
    [c[0] * a, c[1] * a, c[2] * a, c[3] * a]
}
/// `top` over `bot`, both premultiplied
fn over(top: C, bot: C) -> C {
    let k = 1.0 - top[3];
    [top[0] + bot[0] * k, top[1] + bot[1] * k, top[2] + bot[2] * k, top[3] + bot[3] * k]
}
fn norm3(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-6);
    [v[0] / l, v[1] / l, v[2] / l]
}
fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
/// key light from the upper left, slightly in front (x right, y down, z towards viewer)
const L: [f32; 3] = [-0.42, -0.62, 0.66];

fn shade(n: [f32; 3], base: C, amb: f32, spec_k: f32, shin: f32) -> C {
    let n = norm3(n);
    let l = norm3(L);
    let d = dot3(n, l).max(0.0);
    let h = norm3([l[0], l[1], l[2] + 1.0]);
    let s = dot3(n, h).max(0.0).powf(shin) * spec_k;
    add(scale(base, amb + (1.0 - amb) * d * 1.15), s)
}

fn hash(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x8da6b343) ^ (y as u32).wrapping_mul(0xd8163841) ^ seed.wrapping_mul(0xcb1ab31f);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1e995);
    h ^= h >> 15;
    (h & 0xffffff) as f32 / 16777215.0
}
/// periodic value noise, cells wrap at (px, py)
fn vnoise(x: f32, y: f32, px: i32, py: i32, seed: u32) -> f32 {
    let (xi, yi) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (x - x.floor(), y - y.floor());
    let (ux, uy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let g = |i: i32, j: i32| hash(i.rem_euclid(px), j.rem_euclid(py), seed);
    mix(mix(g(xi, yi), g(xi + 1, yi), ux), mix(g(xi, yi + 1), g(xi + 1, yi + 1), ux), uy)
}
fn fbm(x: f32, y: f32, px: i32, py: i32, oct: u32, seed: u32) -> f32 {
    let (mut a, mut f, mut s, mut n) = (0.5, 1, 0.0, 0.0);
    for o in 0..oct {
        s += a * vnoise(x * f as f32, y * f as f32, px * f, py * f, seed + o);
        n += a;
        a *= 0.5;
        f *= 2;
    }
    s / n
}

// ---------------------------------------------------------------- tiles

fn tolex(dir: &Path) {
    let n = 128;
    let cells = 16;
    let h = |x: f32, y: f32| fbm(x * cells as f32 / n as f32, y * cells as f32 / n as f32, cells, cells, 4, 7);
    save(dir, "tolex.png", n, n, |x, y| {
        let e = 0.6;
        let dx = h(x + e, y) - h(x - e, y);
        let dy = h(x, y + e) - h(x, y - e);
        let pebble = h(x, y);
        let base = rgb(0.078, 0.072, 0.066);
        let c = shade([-dx * 9.0, -dy * 9.0, 1.0], base, 0.55, 0.10, 18.0);
        scale(c, 0.85 + pebble * 0.3)
    });
}

fn brass(dir: &Path) {
    let (w, h) = (512, 64);
    save(dir, "brass.png", w, h, |x, y| {
        // horizontal brushing: long streaks, rows decorrelated
        let s = fbm(x / 64.0, y * 1.0, 8, 64, 3, 11) * 0.6 + vnoise(x / 6.0, y * 2.0, 86, 128, 12) * 0.4;
        let k = 0.88 + (s - 0.5) * 0.30;
        scale(rgb(0.80, 0.645, 0.36), k)
    });
}

fn velcro(dir: &Path) {
    let n = 128;
    save(dir, "board.png", n, n, |x, y| {
        let a = fbm(x / 4.0, y / 4.0, 32, 32, 2, 21);
        let b = hash(x as i32, y as i32, 22);
        let v = 0.075 + a * 0.05 + if b > 0.93 { 0.05 } else { 0.0 };
        rgb(v, v, v * 1.05)
    });
}

/// Dither grain laid over large gradients so the 8-bit ramps do not band.
fn grain(dir: &Path) {
    let n = 128;
    save(dir, "grain.png", n, n, |x, y| {
        let g = hash(x as i32, y as i32, 77) - 0.5;
        if g > 0.0 { alpha(rgb(1.0, 1.0, 1.0), g * 0.05) } else { alpha(rgb(0.0, 0.0, 0.0), -g * 0.08) }
    });
}

fn grille(dir: &Path) {
    // basket-weave grille cloth for the head's front band
    let n = 64;
    save(dir, "grille.png", n, n, |x, y| {
        let p = 4.0;
        let cx = (x / p).floor() as i32;
        let cy = (y / p).floor() as i32;
        let fx = x / p - cx as f32;
        let fy = y / p - cy as f32;
        let horiz = (cx + cy) % 2 == 0;
        let t = if horiz { fy } else { fx };
        let ridge = (t * PI).sin();
        let fib = vnoise(x / 1.5, y / 1.5, 43, 43, 31);
        let v = 0.05 + ridge * 0.07 + fib * 0.02;
        rgb(v * 1.05, v, v * 0.95)
    });
}

// ---------------------------------------------------------------- knobs

struct KnobStyle {
    size: usize,
    frames: usize,
    /// cap radius as a fraction of the skirt radius
    cap: f32,
    ridges: f32,
    skirt: C,
    cap_kind: Cap,
    pointer: C,
}
#[derive(Clone, Copy, PartialEq)]
enum Cap {
    SpunBrass,
    BlackGloss,
    Cream,
}

/// Sprite sheet, frame f at y = f * size; angle sweeps -135°..+135° (0 = 12 o'clock, clockwise).
fn knob(dir: &Path, name: &str, k: KnobStyle) {
    let s = k.size as f32;
    let r0 = s / 2.0 - s * 0.07; // skirt radius, leaves room for the shadow
    let rc = r0 * k.cap;
    save(dir, name, k.size, k.size * k.frames, |x, y| {
        let f = (y / s).floor();
        let a = (-135.0 + f / (k.frames - 1) as f32 * 270.0).to_radians();
        let (px, py) = (x - s / 2.0, y - f * s - s / 2.0);
        let r = (px * px + py * py).sqrt();
        let th = px.atan2(-py); // 0 up, clockwise
        // contact shadow, offset down-right
        let (sx, sy) = (px - s * 0.03, py - s * 0.07);
        let sr = (sx * sx + sy * sy).sqrt();
        let mut c = alpha(rgb(0.0, 0.0, 0.0), 0.6 * (1.0 - sstep(r0 * 0.85, r0 * 1.12, sr)));
        if r > r0 {
            return c;
        }
        let (ux, uy) = if r > 1e-3 { (px / r, py / r) } else { (0.0, 0.0) };
        let tk = th - a;
        if r >= rc {
            // knurled skirt: sloped cone, ridges perturb the normal tangentially
            let t = (r - rc) / (r0 - rc);
            let slope = 0.25 + t * 1.1;
            let ridge = (k.ridges * tk).sin() * 0.45 * sstep(0.0, 0.25, t);
            let n = [ux * slope - uy * ridge, uy * slope + ux * ridge, 1.0];
            let mut col = shade(n, k.skirt, 0.30, 0.55, 26.0);
            // rim highlight on the very edge
            col = add(col, 0.10 * sstep(0.85, 1.0, t) * (-(ux * 0.5 + uy * 0.85)).max(0.0));
            // pointer line on the skirt top
            if k.cap_kind == Cap::SpunBrass {
                let d = (px * a.cos() + py * a.sin()).abs(); // distance from pointer ray
                let along = px * a.sin() - py * a.cos();
                if along > 0.0 {
                    let w = 1.0 - sstep(s * 0.012, s * 0.03, d);
                    col = over(alpha(shade([ux * slope, uy * slope, 1.0], k.pointer, 0.55, 0.3, 20.0), w), col);
                }
            }
            c = over(col, c);
            c[3] = c[3].max(1.0 - sstep(r0 - 0.6, r0, r)); // keep the skirt edge opaque over its shadow
            return c;
        }
        // cap
        let t = r / rc;
        let dome = [ux * t * 0.45, uy * t * 0.45, 1.0];
        let mut col = match k.cap_kind {
            Cap::SpunBrass => {
                let rings = fbm(r * 1.7, 0.5, 4096, 1, 2, 5) - 0.5;
                let lt = (-0.62f32).atan2(-0.42).to_degrees(); // light azimuth
                let _ = lt;
                // anisotropic spun-metal highlight: bright wedges facing the light and opposite
                let az = (L[0]).atan2(-L[1]);
                let wedge = (th - az).cos().abs().powf(6.0);
                let base = scale(rgb(0.86, 0.69, 0.38), 0.80 + rings * 0.25);
                add(shade(dome, base, 0.55, 0.35, 30.0), wedge * 0.28 * t)
            }
            Cap::BlackGloss => shade(dome, rgb(0.07, 0.07, 0.07), 0.45, 0.9, 40.0),
            Cap::Cream => shade(dome, rgb(0.93, 0.89, 0.80), 0.55, 0.35, 30.0),
        };
        // bevel ring at the cap edge
        let e = sstep(0.88, 1.0, t);
        col = add(col, e * 0.22 * (-(ux * 0.5 + uy * 0.85)));
        // pointer on cap
        let d = (px * a.cos() + py * a.sin()).abs();
        let along = px * a.sin() - py * a.cos();
        let (lo, hi) = match k.cap_kind {
            Cap::SpunBrass => (rc * 0.15, rc * 0.92),
            _ => (rc * 0.18, rc * 1.0),
        };
        if along > lo && along < hi {
            let w = 1.0 - sstep(s * 0.018, s * 0.045, d);
            let p = if k.cap_kind == Cap::SpunBrass { rgb(0.18, 0.12, 0.05) } else { k.pointer };
            col = over(alpha(p, w), col);
        }
        over(col, c)
    });
}

// ---------------------------------------------------------------- hardware

fn footswitch(dir: &Path) {
    let s = 40.0;
    save(dir, "footswitch.png", 40, 40, |x, y| {
        let (px, py) = (x - s / 2.0, y - s / 2.0);
        let r = (px * px + py * py).sqrt();
        let (sx, sy) = (px - 1.0, py - 2.5);
        let mut c = alpha(rgb(0.0, 0.0, 0.0), 0.65 * (1.0 - sstep(15.0, 19.5, (sx * sx + sy * sy).sqrt())));
        // hex nut
        let hex = {
            let (ax, ay) = (px.abs(), py.abs());
            (ax * 0.866 + ay * 0.5).max(ay)
        };
        if hex < 16.5 {
            let e = sstep(14.5, 16.5, hex);
            let n = [px / 16.5 * e * 1.2, py / 16.5 * e * 1.2, 1.0];
            c = over(shade(n, rgb(0.55, 0.55, 0.56), 0.4, 0.6, 20.0), c);
        }
        // dark gap
        if r < 12.2 {
            c = over(rgb(0.06, 0.06, 0.06), c);
        }
        // chrome dome: environment reflection
        let rd = 11.2;
        if r < rd {
            let t = r / rd;
            let nz = (1.0 - t * t * 0.85).sqrt();
            let n = norm3([px / rd * 0.95, py / rd * 0.95, nz]);
            // reflect view (0,0,1)
            let ry = 2.0 * n[2] * n[1];
            let rx = 2.0 * n[2] * n[0];
            let env = if ry < -0.15 {
                0.95 - (ry + 0.15).abs() * 0.4
            } else if ry < 0.1 {
                0.12
            } else {
                0.35 + ry * 0.35
            };
            let env = env + rx * -0.08;
            let v = env.clamp(0.05, 1.0);
            c = over(rgb(v * 0.97, v * 0.98, v), c);
        }
        c
    });
}

fn led(dir: &Path, name: &str, col: Option<[f32; 3]>) {
    let s = 22.0;
    save(dir, name, 22, 22, |x, y| {
        let (px, py) = (x - s / 2.0, y - s / 2.0);
        let r = (px * px + py * py).sqrt();
        let mut c = [0.0; 4];
        if let Some(k) = col {
            let g = (-(r / 5.5).powi(2)).exp() * 0.55;
            c = alpha(rgb(k[0], k[1], k[2]), g);
        }
        // chrome bezel
        if r < 5.2 && r >= 3.9 {
            let n = [px / r * 0.8, py / r * 0.8, 1.0];
            c = over(shade(n, rgb(0.6, 0.6, 0.6), 0.35, 0.8, 30.0), c);
        }
        if r < 3.9 {
            let lens = match col {
                Some(k) => {
                    let hot = 1.0 - sstep(0.0, 3.2, r);
                    rgb(mix(k[0], 1.0, hot * 0.75), mix(k[1], 1.0, hot * 0.75), mix(k[2], 1.0, hot * 0.75))
                }
                None => rgb(0.10, 0.09, 0.085),
            };
            let (hx, hy) = (px + 1.3, py + 1.4);
            let spec = 1.0 - sstep(0.4, 1.3, (hx * hx + hy * hy).sqrt());
            c = over(add(lens, spec * 0.5), c);
        }
        c
    });
}

fn jewel(dir: &Path, name: &str, on: bool) {
    let s = 40.0;
    save(dir, name, 40, 40, |x, y| {
        let (px, py) = (x - s / 2.0, y - s / 2.0);
        let r = (px * px + py * py).sqrt();
        let th = px.atan2(-py);
        let mut c = [0.0; 4];
        if on {
            c = alpha(rgb(1.0, 0.18, 0.08), (-(r / 12.0).powi(2)).exp() * 0.6);
        }
        if r < 13.0 && r >= 10.0 {
            let n = [px / r, py / r, 0.9];
            c = over(shade(n, rgb(0.62, 0.52, 0.30), 0.35, 0.8, 26.0), c);
        }
        if r < 10.0 {
            // 8 faceted segments + table
            let facet = ((th / (PI / 4.0)).floor() + 0.5) * (PI / 4.0);
            let t = r / 10.0;
            let n = if t < 0.45 { [0.0, 0.0, 1.0] } else { [facet.sin() * 0.7, -facet.cos() * 0.7, 1.0] };
            let base = if on { rgb(0.95, 0.12, 0.05) } else { rgb(0.28, 0.04, 0.03) };
            let mut col = shade(n, base, if on { 0.85 } else { 0.4 }, 0.9, 40.0);
            if on {
                col = add(col, (1.0 - sstep(0.0, 0.5, t)) * 0.35);
            }
            c = over(col, c);
        }
        c
    });
}

fn jack(dir: &Path) {
    let s = 26.0;
    save(dir, "jack.png", 26, 26, |x, y| {
        let (px, py) = (x - s / 2.0, y - s / 2.0);
        let r = (px * px + py * py).sqrt();
        let mut c = alpha(rgb(0.0, 0.0, 0.0), 0.5 * (1.0 - sstep(9.0, 12.5, ((px - 1.0).powi(2) + (py - 2.0).powi(2)).sqrt())));
        let hex = {
            let (ax, ay) = (px.abs(), py.abs());
            (ax * 0.866 + ay * 0.5).max(ay)
        };
        if hex < 10.5 {
            let e = sstep(8.5, 10.5, hex);
            c = over(shade([px / 10.5 * e, py / 10.5 * e, 1.0], rgb(0.62, 0.62, 0.63), 0.45, 0.7, 24.0), c);
        }
        if r < 7.0 {
            let n = [px / 7.0 * 0.7, py / 7.0 * 0.7, 1.0];
            c = over(shade(n, rgb(0.5, 0.5, 0.52), 0.4, 0.8, 30.0), c);
        }
        if r < 4.2 {
            c = over(rgb(0.02, 0.02, 0.02), c);
        }
        c
    });
}

/// Shading for a compact pedal enclosure (W x H, corner radius R). Transparent where the body
/// colour should show; white = light, black = shadow.
fn pedal_overlay(dir: &Path, w: usize, h: usize, rad: f32, groove: f32) {
    let (wf, hf) = (w as f32, h as f32);
    save(dir, "pedal.png", w, h, |x, y| {
        // rounded-rect SDF (positive inside)
        let qx = (x - wf / 2.0).abs() - (wf / 2.0 - rad);
        let qy = (y - hf / 2.0).abs() - (hf / 2.0 - rad);
        let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - rad;
        let d = -outside;
        if d < 0.0 {
            return [0.0; 4];
        }
        let mut c = [0.0f32; 4];
        let white = |a: f32| alpha(rgb(1.0, 1.0, 1.0), a.clamp(0.0, 1.0));
        let black = |a: f32| alpha(rgb(0.0, 0.0, 0.0), a.clamp(0.0, 1.0));
        // body curvature: light at top, falloff to the bottom
        let t = y / hf;
        c = over(black(0.32 * t * t), c);
        c = over(white(0.16 * (1.0 - t).powi(3)), c);
        // diagonal sheen band
        let band = (x * 0.55 + y - hf * 0.18) / (hf * 0.10);
        c = over(white(0.07 * (-band * band).exp()), c);
        // powder-coat grain
        let g = hash(x as i32, y as i32, 99) - 0.5;
        c = over(if g > 0.0 { white(g * 0.07) } else { black(-g * 0.10) }, c);
        // edge bevel: SDF gradient as outward normal
        let e = 0.5;
        let sd = |x: f32, y: f32| {
            let qx = (x - wf / 2.0).abs() - (wf / 2.0 - rad);
            let qy = (y - hf / 2.0).abs() - (hf / 2.0 - rad);
            (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - rad
        };
        let (gx, gy) = (sd(x + e, y) - sd(x - e, y), sd(x, y + e) - sd(x, y - e));
        let gl = (gx * gx + gy * gy).sqrt().max(1e-4);
        let lit = -(gx / gl * -0.45 + gy / gl * -0.89); // facing the light
        let b = 1.0 - sstep(0.0, 4.0, d);
        if lit < 0.0 {
            c = over(white(0.55 * b * -lit), c);
        } else {
            c = over(black(0.6 * b * lit), c);
        }
        c = over(black(0.8 * (1.0 - sstep(0.0, 0.9, d))), c); // crisp outline
        // hinge groove between control deck and treadle
        if y >= groove && y < groove + 1.0 {
            c = over(black(0.45), c);
        } else if y >= groove + 1.0 && y < groove + 2.0 {
            c = over(white(0.16), c);
        }
        // screws in the bottom corners
        for (sx, sy) in [(7.0, hf - 7.0), (wf - 7.0, hf - 7.0)] {
            let (dx, dy) = (x - sx, y - sy);
            let r = (dx * dx + dy * dy).sqrt();
            if r < 2.8 {
                let n = [dx / 2.8 * 0.8, dy / 2.8 * 0.8, 1.0];
                let mut sc = shade(n, rgb(0.55, 0.55, 0.56), 0.4, 0.8, 24.0);
                if dx.abs() < 0.45 || dy.abs() < 0.45 {
                    sc = rgb(0.12, 0.12, 0.12);
                }
                c = over(sc, c);
            }
        }
        c
    });
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("tex") {
        let out = std::env::args().nth(2).unwrap_or_else(|| "crates/katana-studio/ui/tex".into());
        std::fs::create_dir_all(&out).unwrap();
        return tex::run(Path::new(&out));
    }
    let out = std::env::args().nth(1).unwrap_or_else(|| "crates/katana-studio/ui/art".into());
    let dir = Path::new(&out);
    std::fs::create_dir_all(dir).unwrap();
    tolex(dir);
    brass(dir);
    velcro(dir);
    grille(dir);
    grain(dir);
    knob(
        dir,
        "knob-amp.png",
        KnobStyle { size: 60, frames: 64, cap: 0.60, ridges: 30.0, skirt: rgb(0.075, 0.072, 0.07), cap_kind: Cap::SpunBrass, pointer: rgb(0.95, 0.92, 0.85) },
    );
    knob(
        dir,
        "knob-black.png",
        KnobStyle { size: 26, frames: 64, cap: 0.70, ridges: 18.0, skirt: rgb(0.06, 0.06, 0.06), cap_kind: Cap::BlackGloss, pointer: rgb(0.96, 0.96, 0.96) },
    );
    knob(
        dir,
        "knob-cream.png",
        KnobStyle { size: 26, frames: 64, cap: 0.70, ridges: 18.0, skirt: rgb(0.06, 0.06, 0.06), cap_kind: Cap::Cream, pointer: rgb(0.08, 0.08, 0.08) },
    );
    knob(
        dir,
        "knob-insp.png",
        KnobStyle { size: 48, frames: 64, cap: 0.68, ridges: 24.0, skirt: rgb(0.06, 0.06, 0.06), cap_kind: Cap::BlackGloss, pointer: rgb(0.96, 0.96, 0.96) },
    );
    footswitch(dir);
    led(dir, "led-off.png", None);
    led(dir, "led-green.png", Some([0.27, 0.95, 0.45]));
    led(dir, "led-red.png", Some([1.0, 0.25, 0.18]));
    led(dir, "led-yellow.png", Some([1.0, 0.78, 0.18]));
    jewel(dir, "jewel-on.png", true);
    jewel(dir, "jewel-off.png", false);
    jack(dir);
    pedal_overlay(dir, 92, 176, 7.0, 104.0);
    stage_light(dir);
}

/// Stage lighting overlay, stretched over the whole stage: a warm key light from
/// above the amp plus a dark vignette. Per-pixel dither so the stretched,
/// low-contrast falloff does not band.
fn stage_light(dir: &Path) {
    let (w, h) = (640usize, 400usize);
    save(dir, "stage-light.png", w, h, |x, y| {
        let u = x / w as f32;
        let v = y / h as f32;
        // key light: wide ellipse centred on the amp head (upper half)
        let dx = (u - 0.5) / 0.36;
        let dy = (v - 0.36) / 0.40;
        let key = (-(dx * dx + dy * dy)).exp();
        // vignette: corners and bottom fall to black
        let ex = (u - 0.5) * 2.0;
        let ey = (v - 0.42) * 1.9;
        let vig = sstep(0.55, 1.35, (ex * ex + ey * ey).sqrt());
        let d = (hash(x.floor() as i32, y.floor() as i32, 911) - 0.5) / 255.0 * 1.6;
        let light = alpha(rgb(1.0, 0.80, 0.52), (key * 0.11 + d).max(0.0));
        let dark = alpha(rgb(0.0, 0.0, 0.0), (vig * 0.62 + d).clamp(0.0, 1.0));
        over(light, dark)
    });
}
