//! `haze --png out.png [--tab N]`: render one frame offscreen with the same software
//! renderer the window uses. Visual regression proof without touching the desktop.

use crate::MainWindow;
use slint::ComponentHandle;
use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::PhysicalSize;
use std::rc::Rc;

pub(crate) struct Offscreen(pub(crate) Rc<MinimalSoftwareWindow>);

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

pub fn render(lib: katana_core::Library, out: &str, tab: i32, hover: Option<(f32, f32)>, w: u32, h: u32) {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen(window.clone()))).expect("platform");
    window.set_size(PhysicalSize::new(w, h));
    let ui = MainWindow::new().expect("window");
    crate::app::App::install(&ui, lib);
    if std::env::var_os("HAZE_FAKE_AMP").is_some() {
        // layout check of the connected state (channel bar) without an amp
        let names = ["PANEL", "A1", "A2", "A3", "A4", "B1", "B2", "B3", "B4"];
        let v: Vec<crate::Slot> = names
            .iter()
            .enumerate()
            .map(|(i, l)| crate::Slot { label: (*l).into(), name: format!("Channel {l}").into(), current: i == 0 })
            .collect();
        ui.set_slots(slint::ModelRc::new(slint::VecModel::from(v)));
        ui.set_connected(true);
    }
    if tab >= 0 {
        ui.invoke_pedal_clicked(tab); // open the inspector of pedal #tab
    }
    // HAZE_PAGE=1|2: Library / Setlists tab; HAZE_PICKER=1: quick preset picker window
    if let Some(p) = std::env::var("HAZE_PAGE").ok().and_then(|v| v.parse().ok()) {
        ui.invoke_page_picked(p);
    }
    if std::env::var_os("HAZE_PICKER").is_some() {
        ui.set_show_picker(true);
    }
    // HAZE_LIVE=n: stage view, n songs stepped in (0 = nothing selected yet)
    if let Some(n) = std::env::var("HAZE_LIVE").ok().and_then(|v| v.parse::<i32>().ok()) {
        ui.set_live_open(true);
        for _ in 0..n {
            ui.invoke_tone_step(1);
        }
    }
    // HAZE_SCALE=125: interface size (same event the settings dispatch)
    if let Some(z) = std::env::var("HAZE_SCALE").ok().and_then(|v| v.parse::<f32>().ok()) {
        ui.window().dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: z / 100.0 });
    }
    if std::env::var_os("HAZE_KEYS").is_some() {
        ui.set_show_keys(true);
    }
    // HAZE_ADD=i: "add to setlist" dialog for visible tone #i
    if let Some(i) = std::env::var("HAZE_ADD").ok().and_then(|v| v.parse().ok()) {
        ui.set_add_tone(i);
    }
    ui.show().unwrap();
    if let Some((x, y)) = hover {
        // first frame so the item tree has geometry, then move the pointer over it
        slint::platform::update_timers_and_animations();
        window.draw_if_needed(|_| {});
        window.dispatch_event(slint::platform::WindowEvent::PointerMoved { position: slint::LogicalPosition::new(x, y) });
    }
    let mut buf = vec![PremultipliedRgbaColor::default(); (w * h) as usize];
    for _ in 0..4 {
        std::thread::sleep(std::time::Duration::from_millis(80)); // let the drawer animation settle
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut buf, w as usize);
        });
    }
    let rgba: Vec<u8> = buf.iter().flat_map(|p| [p.red, p.green, p.blue, 255]).collect();
    std::fs::write(out, png(w, h, &rgba)).expect("write png");
    let lit = rgba.chunks(4).filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 60).count();
    println!("{out}: {w}x{h}, non-background pixels: {lit}");
}

/// Minimal PNG encoder (stored deflate blocks) — avoids an image crate dependency.
fn png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    fn crc(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, ty: &[u8], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut c = ty.to_vec();
        c.extend_from_slice(data);
        out.extend_from_slice(&c);
        out.extend_from_slice(&crc(&c).to_be_bytes());
    }
    let mut raw = Vec::with_capacity(rgba.len() + h as usize);
    for row in rgba.chunks(w as usize * 4) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, b) in blocks.iter().enumerate() {
        z.push((i == blocks.len() - 1) as u8);
        z.extend_from_slice(&(b.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(b.len() as u16)).to_le_bytes());
        z.extend_from_slice(b);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = vec![];
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}
