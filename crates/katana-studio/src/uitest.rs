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

    drop(ui);
    let _ = std::fs::remove_dir_all(&root);
}
