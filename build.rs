// Windows: the gui stack links many system DLLs (OpenGL, DirectX, DirectWrite,
// UI Automation, COM, ...). Loading them at startup costs ~70 ms on every run,
// even for programs that never open a window, so they are delay-loaded: each
// is loaded the first time one of its functions is called.

fn main() {
    let windows_msvc = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if windows_msvc && std::env::var_os("CARGO_FEATURE_GUI").is_some() {
        for dll in [
            "opengl32.dll",
            "dxgi.dll",
            "dwrite.dll",
            "uiautomationcore.dll",
            "dwmapi.dll",
            "uxtheme.dll",
            "imm32.dll",
            "setupapi.dll",
            "shell32.dll",
            "shlwapi.dll",
            "ole32.dll",
            "oleaut32.dll",
            "combase.dll",
            "gdi32.dll",
        ] {
            println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{}", dll);
        }
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
