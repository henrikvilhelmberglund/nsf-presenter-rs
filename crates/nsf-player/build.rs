#[cfg(windows)]
extern crate winres;

use std::path::Path;

fn compile(path: &str) {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let config = slint_build::CompilerConfiguration::new()
        .with_include_paths(vec![manifest_dir.join("assets")])
        .with_style("fluent-dark".to_string());
    slint_build::compile_with_config(path, config).unwrap();
}

#[cfg(windows)]
fn apply_windows_resources() {
    let mut res = winres::WindowsResource::new();
    res.set_icon("assets/nsf-presenter-icon.ico");
    res.set_manifest(
        r#"
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0" xmlns:asmv3="urn:schemas-microsoft-com:asm.v3">
    <asmv3:application>
        <asmv3:windowsSettings xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">
            <dpiAwareness>PerMonitorV2, PerMonitor, System, unaware</dpiAwareness>
        </asmv3:windowsSettings>
    </asmv3:application>
</assembly>
    "#,
    );
    res.compile().unwrap();
}

#[cfg(not(windows))]
fn apply_windows_resources() {}

fn main() {
    apply_windows_resources();
    // player.slint comes first so SLINT_INCLUDE_GENERATED points at
    // visualization.rs after the last compile() call (consumed by
    // slint::include_modules!() in main.rs). player.rs is included
    // explicitly via `include!(concat!(env!("OUT_DIR"), "/player.rs"))`.
    compile("src/slint/player.slint");
    compile("src/slint/visualization.slint");
}
