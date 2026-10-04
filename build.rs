// build.rs — compile the C parser inline and link the zig static lib
// (which the top-level build script produces before cargo runs).

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn embed_windows_icon() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let target = env::var("TARGET").expect("Cargo TARGET is missing");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo OUT_DIR is missing"))
        .join("openterm-icon.res");
    let mut command = if let Some(tool) = cc::windows_registry::find_tool(&target, "rc.exe") {
        tool.to_command()
    } else {
        Command::new("llvm-rc")
    };
    let status = command
        .arg("/nologo")
        .arg("/fo")
        .arg(&output)
        .arg("assets/windows/openterm.rc")
        .status()
        .expect("run a Windows resource compiler for the OpenTerm icon");
    assert!(status.success(), "Windows icon resource compilation failed");

    // MSVC link.exe and lld-link accept compiled .res files directly.
    println!("cargo:rustc-link-arg-bin=openterm={}", output.display());
}

fn main() {
    // Explorer, taskbar and shortcut icon for the Windows executable.
    embed_windows_icon();

    // --- C: vt parser --------------------------------------------------------
    cc::Build::new()
        .file("native/vtparse.c")
        .include("native")
        .opt_level(3)
        .flag_if_supported("-ffunction-sections")
        .flag_if_supported("-fdata-sections")
        .warnings(true)
        .compile("vtparse");

    // --- Zig: fastgrid static lib -------------------------------------------
    // the parent build script (build.bat / build.sh) emits:
    //   target/native/libfastgrid.a   (unix)
    //   target/native/fastgrid.lib    (windows)
    // we just tell rustc to link against whichever exists.
    let root = env::var("CARGO_MANIFEST_DIR").unwrap();
    let native_dir = PathBuf::from(&root).join("target").join("native");
    println!("cargo:rustc-link-search=native={}", native_dir.display());

    println!("cargo:rustc-link-lib=static=fastgrid");

    // re-run build when any native source changes
    println!("cargo:rerun-if-changed=native/vtparse.c");
    println!("cargo:rerun-if-changed=native/vtparse.h");
    println!("cargo:rerun-if-changed=native/fastgrid.zig");
    println!("cargo:rerun-if-changed=assets/windows/openterm.rc");
    println!("cargo:rerun-if-changed=assets/icons/openterm.ico");
}
