//! Headless interaction test: the real window component and `App` on an offscreen platform with a
//! temporary library. Drives the same callbacks the widgets fire (no pixels involved) and checks
//! what the UI shows and what lands on disk.

use crate::snapshot::Offscreen;
use crate::MainWindow;
use katana_core::Library;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::Model;

fn names(ui: &MainWindow) -> Vec<String> {
    ui.get_tones().iter().map(|t| t.name.to_string()).collect()
}

fn pedal(ui: &MainWindow, title: &str) -> i32 {
    ui.get_pedals().iter().position(|p| p.title.eq_ignore_ascii_case(title)).expect(title) as i32
}

#[test]
fn ui_flows() {
    std::env::set_var("KATANA_NO_DEVICE", "1");
    let root = std::env::temp_dir().join(format!("haze-uitest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let lib = Library::open(&root).unwrap();
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen(window.clone()))).unwrap();
    window.set_size(slint::PhysicalSize::new(1600, 1000));
    let ui = MainWindow::new().unwrap();
    crate::app::App::install(&ui, lib);
    assert!(!ui.get_has_selection(), "empty library: nothing loaded");

    // ---- starter rigs land in the library and load on the rig
    ui.invoke_page_picked(3);
    for i in 0..3 {
        ui.invoke_template_use(i);
    }
    assert_eq!(ui.get_page(), 0);
    assert!(ui.get_has_selection());
    assert_eq!(ui.get_ed_name(), "Classic Rock");
    ui.invoke_template_use(2);
    assert_eq!(Library::open(&root).unwrap().tones.len(), 3, "using a rig twice does not duplicate it");

    // ---- library: name sort, prev/next wraps
    ui.invoke_page_picked(1);
    ui.invoke_sort_picked(0);
    assert_eq!(names(&ui), ["Blues Breaker", "Classic Rock", "Glass Clean"]);
    ui.invoke_tone_clicked(2);
    ui.invoke_tone_step(1);
    assert_eq!(ui.get_ed_name(), "Blues Breaker", "next after the last wraps to the first");
    ui.invoke_tone_step(-1);
    assert_eq!(ui.get_ed_name(), "Glass Clean");

    // ---- pedal knob + undo / redo
    let amp = pedal(&ui, "Amp");
    assert!(!ui.get_ed_dirty());
    ui.invoke_pedal_knob_step(amp, 0, 1);
    assert!(ui.get_ed_dirty() && ui.get_can_undo() && !ui.get_can_redo());
    ui.invoke_undo();
    assert!(!ui.get_ed_dirty(), "undo restores the saved patch");
    assert!(ui.get_can_redo());
    ui.invoke_redo();
    assert!(ui.get_ed_dirty());
    ui.invoke_revert();
    assert!(!ui.get_ed_dirty());
    ui.invoke_pedal_knob_set(amp, 2, 1.0);
    assert_eq!(ui.get_pedals().row_data(amp as usize).unwrap().knobs.row_data(2).unwrap().norm, 1.0, "knob drag reaches the face");
    ui.invoke_revert();

    // ---- inspector extras: delay tempo sync, EQ curve
    ui.invoke_pedal_clicked(pedal(&ui, "Delay"));
    assert_eq!(ui.get_insp_extra(), 2);
    ui.invoke_insp_delay_sync(375);
    assert_eq!(ui.get_insp_delay_ms(), 375);
    ui.invoke_pedal_clicked(pedal(&ui, "EQ"));
    assert_eq!(ui.get_insp_extra(), 1);
    assert!(ui.get_insp_eq_curve().starts_with("M0.0 "));
    ui.invoke_revert();

    // ---- setlists: new, add current, add from a card, reorder, remove
    ui.invoke_page_picked(2);
    ui.invoke_set_new();
    let set = ui.get_nav_selected().to_string();
    assert_eq!(ui.get_set_count(), 1);
    ui.invoke_set_add_current();
    assert_eq!(names(&ui), ["Glass Clean"]);
    ui.invoke_set_add_current();
    assert_eq!(ui.get_tones().row_count(), 1, "no duplicates in a setlist");
    ui.invoke_page_picked(1);
    ui.invoke_sort_picked(0);
    ui.invoke_tone_add_to(0, set.clone().into()); // Blues Breaker
    ui.invoke_tone_add_to(1, "".into()); // Classic Rock into a new setlist
    assert_eq!(ui.get_set_count(), 2);
    ui.invoke_nav_clicked(set.clone().into());
    assert_eq!(names(&ui), ["Glass Clean", "Blues Breaker"]);
    ui.invoke_song_move(1, -1);
    assert_eq!(names(&ui), ["Blues Breaker", "Glass Clean"]);
    ui.invoke_song_remove(0);
    assert_eq!(names(&ui), ["Glass Clean"]);
    let disk = Library::open(&root).unwrap();
    assert_eq!(disk.setlists.iter().find(|s| s.id == set).unwrap().tones.len(), 1, "setlist edits are saved");

    // ---- live view steps through the setlist
    ui.set_live_open(true);
    ui.invoke_tone_step(1);
    assert_eq!(ui.get_selected_index(), 0);

    // ---- user tags: chips, filter, search
    ui.invoke_page_picked(1);
    ui.invoke_tone_clicked(0); // Blues Breaker
    ui.invoke_set_tags("blues, Live ,#live".into());
    assert_eq!(ui.get_ed_tags(), "blues, Live, live");
    let tags: Vec<String> = ui.get_user_tags().iter().map(|t| t.to_string()).collect();
    assert_eq!(tags, ["blues", "Live", "live"]);
    ui.invoke_tag_picked("#blues".into());
    assert_eq!(names(&ui), ["Blues Breaker"]);
    ui.invoke_set_tags("".into());
    assert_eq!(ui.get_tag(), "", "filter on a tag nobody has is dropped");
    assert_eq!(ui.get_tones().row_count(), 3);

    // ---- November Rain starter: what the PC engine plays
    {
        use haze_dsp::{EqKey, Params, P};
        ui.invoke_page_picked(3);
        let i = crate::templates::TEMPLATES.iter().position(|t| t.name == "November Rain").unwrap();
        ui.invoke_template_use(i as i32);
        assert_eq!(ui.get_ed_name(), "November Rain");
        let d = crate::app::dsp_params();
        let eq = |k| d.get_i(Params::eq(0, k));
        let line = format!(
            "voicing {} gain {:.2} mid {:.2} | drive on {} model {} amt {:.2} | guitar sim on {} | eq on {} pre {} lc {} lm {}/{} hm {}/{} hc {} | delay {} ms fb {:.2} wet {:.2} | reverb type {} {:.1} s wet {:.2} | gate {} {:.0} dB",
            d.get(P::AmpVoicing), d.get(P::AmpGain), d.get(P::AmpMid),
            d.get(P::DriveOn), d.get(P::DriveModel), d.get(P::DriveAmt),
            d.get_i(Params::fx(1, EqKey::On)),
            eq(EqKey::On), eq(EqKey::Pre), eq(EqKey::V(0)), eq(EqKey::V(2)), eq(EqKey::V(4)), eq(EqKey::V(5)), eq(EqKey::V(7)), eq(EqKey::V(9)),
            d.get(P::DelayMs), d.get(P::DelayFeedback), d.get(P::DelayMix),
            d.get(P::ReverbType), d.get(P::ReverbTime), d.get(P::ReverbMix),
            d.get(P::GateOn), d.get(P::GateThreshDb),
        );
        eprintln!("November Rain on the PC engine: {line}");
        assert_eq!(d.get(P::AmpVoicing), 4.0, "LEAD = the Marshall lead voicing");
        assert_eq!((d.get(P::DriveOn), d.get(P::DriveModel)), (1.0, 7.0), "MID BOOST stays a clean mid boost");
        assert_eq!(d.get_i(Params::fx(1, EqKey::On)), 0.0, "guitar sim waits for single coils");
        assert_eq!((eq(EqKey::On), eq(EqKey::Pre), eq(EqKey::V(4)), eq(EqKey::V(7))), (1.0, 0.0, -3.0, 3.0));
        assert_eq!((d.get(P::DelayOn), d.get(P::DelayMs)), (1.0, 375.0));
        assert_eq!((d.get(P::ReverbOn), d.get(P::ReverbType)), (1.0, 3.0));
    }

    // ---- every starter on the PC engine: loudness close to the others
    {
        ui.invoke_page_picked(3);
        let mut rows = vec![];
        for (i, t) in crate::templates::TEMPLATES.iter().enumerate() {
            ui.invoke_template_use(i as i32);
            rows.push((t.name, t.style, starter_loudness(&crate::app::dsp_params())));
        }
        let mut sorted: Vec<f32> = rows.iter().map(|r| r.2).collect();
        sorted.sort_by(f32::total_cmp);
        let median = sorted[sorted.len() / 2];
        for (name, style, l) in &rows {
            eprintln!("starter {name:<16} {style:<8} {l:+6.1} LUFS ({:+.1} dB vs median)", l - median);
        }
        for (name, _, l) in &rows {
            assert!((l - median).abs() <= 2.5, "{name}: {l:.1} LUFS, median {median:.1}: starters must sit within 2.5 dB");
        }
    }

    // ---- practice page: user groove grid, mixer, kit
    ui.invoke_page_picked(6);
    assert_eq!(ui.get_page(), 6);
    let cell = |r: usize, st: usize| ui.get_drum_rows().row_data(r).unwrap().cells.row_data(st).unwrap();
    ui.invoke_drum_pattern_picked(8); // Disco: shown, read-only
    assert_eq!(cell(0, 4), 3, "disco kick on beat 2");
    ui.invoke_drum_cell(0, 1, -1);
    assert_eq!(cell(0, 1), 0, "presets do not edit");
    ui.invoke_user_slot_picked(1);
    assert_eq!(ui.get_drum_pattern(), haze_dsp::practice::USER as i32);
    assert_eq!(ui.get_user_slot(), 1);
    assert!((0..16).all(|st| cell(0, st) == 0), "U2 starts empty");
    ui.invoke_drum_cell(0, 2, -1);
    ui.invoke_drum_cell(0, 2, -1);
    assert_eq!(cell(0, 2), 3, "rest → normal → accent");
    ui.invoke_drum_cell(0, 2, 0);
    assert_eq!(cell(0, 2), 0, "right-click clears");
    ui.invoke_user_len_picked(12);
    assert_eq!((ui.get_bar_len(), ui.get_drum_rows().row_data(0).unwrap().cells.row_count()), (12, 12));
    ui.invoke_drum_pattern_picked(3);
    ui.invoke_user_copy();
    assert_eq!(ui.get_user_slot(), 1);
    assert_eq!(ui.get_user_len(), 16, "a copied 16-step groove brings its length");
    assert_eq!(cell(1, 4), 3, "funk snare copied into U2");
    let bus = |i: usize| ui.get_mix_bus().row_data(i).unwrap();
    assert_eq!(bus(0).db, "0.0");
    ui.invoke_mix_set(0, 0.5);
    assert_eq!(bus(0).db, "−6.1");
    ui.invoke_mix_mute(0);
    assert!(bus(0).mute);
    ui.invoke_mix_reset(0);
    assert_eq!(bus(0).db, "0.0");
    ui.invoke_mix_set(5, 0.0);
    assert_eq!(ui.get_mix_kit().row_data(2).unwrap().db, "−∞", "hats fader down");
    ui.invoke_kit_set(0, 9);
    assert_eq!(ui.get_kit_tune(), 6, "tune clamps to ±6 st");
    ui.invoke_kit_set(4, 0);
    assert_eq!(ui.get_kit_fills(), 0);

    drop(ui);
    let _ = std::fs::remove_dir_all(&root);
}

/// K-weighted loudness (LUFS-like, ungated) of the DI riff through a copy of the engine params.
fn starter_loudness(live: &haze_dsp::Params) -> f32 {
    use haze_dsp::filters::Biquad;
    let fs = 48000.0;
    let p = std::sync::Arc::new(haze_dsp::Params::default());
    for i in 0..haze_dsp::params::COUNT {
        p.set_i(i, live.get_i(i));
    }
    let out = haze_dsp::render_offline(p, fs, 9.6);
    let skip = (0.5 * fs) as usize;
    let mut ms = 0f64;
    for ch in 0..2 {
        let (mut hs, mut hp) = (Biquad::new(), Biquad::new());
        hs.high_shelf(1681.0, 4.0, fs);
        hp.highpass(38.0, 0.5, fs);
        for (i, s) in out.iter().skip(ch).step_by(2).enumerate() {
            let k = hp.tick(hs.tick(*s));
            if i >= skip {
                ms += (k * k) as f64;
            }
        }
    }
    (-0.691 + 10.0 * (ms / (out.len() / 2 - skip) as f64).log10()) as f32
}
