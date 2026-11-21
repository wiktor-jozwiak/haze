use katana_core::map::{Key, ParamRef};
use katana_core::patch::Patch;
use katana_core::Library;

fn set(p: &mut Patch, block: &str, prm: &'static str, val: i16) {
    if let Some(r) = ParamRef::find(block, Key::Prm(prm)) {
        p.set(r, val);
    } else {
        eprintln!("Warning: parameter not found: {}/{}", block, prm);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut patch = Patch::default();
    patch.set_name("Until I Found U");

    // 1. Preamp: Fender-style Clean (Clean / Clean Twin)
    set(&mut patch, "Patch_0", "PRM_PREAMP_A_TYPE", 8); // Clean (or 9 for Clean Twin)
    set(&mut patch, "Patch_0", "PRM_PREAMP_A_GAIN", 40);
    set(&mut patch, "Patch_0", "PRM_PREAMP_A_BASS", 46);
    set(&mut patch, "Patch_0", "PRM_PREAMP_A_MIDDLE", 68);
    set(&mut patch, "Patch_0", "PRM_PREAMP_A_TREBLE", 54);
    set(&mut patch, "Patch_0", "PRM_PREAMP_A_PRESENCE", 42);
    set(&mut patch, "Patch_0", "PRM_PREAMP_A_LEVEL", 55);
    set(&mut patch, "Patch_0", "PRM_PREAMP_A_BRIGHT", 0);

    // 2. Booster: Warm OD (subtle warm tube saturation)
    set(&mut patch, "Patch_0", "PRM_ODDS_SW", 1);
    set(&mut patch, "Patch_0", "PRM_ODDS_TYPE", 5); // Warm OD
    set(&mut patch, "Patch_0", "PRM_ODDS_DRIVE", 10);
    set(&mut patch, "Patch_0", "PRM_ODDS_TONE", 0);
    set(&mut patch, "Patch_0", "PRM_ODDS_EFFECT_LEVEL", 50);

    // 3. Reverb: Spring Reverb ('63 Spring)
    set(&mut patch, "Patch_1", "PRM_REVERB_SW", 1);
    set(&mut patch, "Patch_1", "PRM_REVERB_TYPE", 5); // Spring
    set(&mut patch, "Patch_1", "PRM_REVERB_TIME", 34);
    set(&mut patch, "Patch_1", "PRM_REVERB_PREDELAY", 8);
    set(&mut patch, "Patch_1", "PRM_REVERB_LOWCUT", 2);
    set(&mut patch, "Patch_1", "PRM_REVERB_HICUT", 10);
    set(&mut patch, "Patch_1", "PRM_REVERB_EFFECT_LEVEL", 44);
    set(&mut patch, "Patch_1", "PRM_REVERB_SPRING_COLOR", 70);

    // 4. Delay: Slapback Tape Echo
    set(&mut patch, "Delay(1)", "PRM_DLY_SW", 1);
    set(&mut patch, "Delay(1)", "PRM_DLY_TYPE", 8); // Tape Echo
    set(&mut patch, "Delay(1)", "PRM_DLY_COMMON_DLY_TIME", 120); // 120ms slapback
    set(&mut patch, "Delay(1)", "PRM_DLY_COMMON_FEEDBACK", 14); // 1 repeat
    set(&mut patch, "Delay(1)", "PRM_DLY_COMMON_EFFECT_LEVEL", 32);
    set(&mut patch, "Delay(1)", "PRM_DLY_COMMON_HICUT", 8);

    // 5. Mod: Tremolo (slow & subtle vintage vibe)
    set(&mut patch, "Fx(1)", "PRM_FX1_SW", 1);
    set(&mut patch, "Fx(1)", "PRM_FX1_FXTYPE", 21); // Tremolo
    set(&mut patch, "Fx(1)", "PRM_FX1_TREMOLO_WAVESHAPE", 50);
    set(&mut patch, "Fx(1)", "PRM_FX1_TREMOLO_RATE", 35);
    set(&mut patch, "Fx(1)", "PRM_FX1_TREMOLO_DEPTH", 25);
    set(&mut patch, "Fx(1)", "PRM_FX1_TREMOLO_LEVEL", 50);

    // 6. FX (Fx2): Compressor (Light Comp for vintage sustain/smoothness)
    set(&mut patch, "Fx(2)", "PRM_FX1_SW", 1);
    set(&mut patch, "Fx(2)", "PRM_FX1_FXTYPE", 3); // Comp
    set(&mut patch, "Fx(2)", "PRM_FX1_ADCOMP_TYPE", 2); // Light
    set(&mut patch, "Fx(2)", "PRM_FX1_ADCOMP_SUSTAIN", 38);
    set(&mut patch, "Fx(2)", "PRM_FX1_ADCOMP_ATTACK", 50);
    set(&mut patch, "Fx(2)", "PRM_FX1_ADCOMP_TONE", 0);
    set(&mut patch, "Fx(2)", "PRM_FX1_ADCOMP_LEVEL", 50);

    // 7. Noise Suppressor
    set(&mut patch, "Patch_1", "PRM_NS1_SW", 1);
    set(&mut patch, "Patch_1", "PRM_NS1_THRESHOLD", 30);
    set(&mut patch, "Patch_1", "PRM_NS1_RELEASE", 50);

    // Open user library and add patch
    let root = Library::default_root();
    println!("Opening library at: {:?}", root);
    let mut lib = Library::open(&root)?;

    let memo = "Stephen Sanchez - Until I Found You (Live 1:1 Clean Tone). Vintage Fender Clean, Slapback Tape Echo 120ms, 63 Spring Reverb, Warm OD boost, subtle Tremolo and Light Comp.";
    let (id, is_new) = lib.add_patch(patch.clone(), "Haze Studio", memo)?;

    // Add tags and mark as favorite if desired
    lib.update_tone(&id, |t| {
        t.tags = vec!["clean".into(), "vintage".into(), "live".into(), "ballad".into()];
        t.memo = memo.into();
    })?;

    println!("Patch saved successfully!");
    println!("ID: {}", id);
    println!("Is new: {}", is_new);
    println!("Tone name: {}", patch.name());
    println!("Summary: {:?}", patch.summary());

    Ok(())
}
