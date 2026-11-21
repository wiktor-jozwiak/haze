fn main() {
    let cfg = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into());
    slint_build::compile_with_config("ui/main.slint", cfg).expect("slint compile");

    // Windows exe: Explorer/taskbar icon and file properties (the window icon comes from main.slint)
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../packaging/haze.ico")
            .set("ProductName", "Haze")
            .set("FileDescription", "Haze: tone editor for BOSS KATANA MkII");
        res.compile().expect("windows resources");
    }
}
