//! The person segmentation model as Burn code and weights (`$OUT_DIR/model/selfie_segmenter_landscape.{rs,bpk}`),
//! generated from the ONNX the CPU path runs, for the GPU path (`src/video/effects/gpu.rs`). One model file, two
//! runtimes: the generated code is never edited or committed.
//!
//! It also stops a macOS build that links without `-ObjC` ([`require_objc_link_on_macos`]).

const MODEL: &str = "assets/segmentation/selfie_segmenter_landscape.onnx";

fn main() {
    println!("cargo::rerun-if-changed={MODEL}");
    require_objc_link_on_macos();
    #[cfg(feature = "effects")]
    burn_onnx::ModelGen::new()
        .input(MODEL)
        .out_dir("model/")
        .run_from_script();
}

/// A RUSTFLAGS from the environment replaces the `-ObjC` of `.cargo/config.toml`. A macOS binary without it aborts at
/// its first call, so a build without it stops here.
fn require_objc_link_on_macos() {
    println!("cargo::rerun-if-env-changed=RUSTFLAGS");
    println!("cargo::rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let flags = std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();
    if !flags.split('\x1f').any(|flag| flag.contains("-ObjC")) {
        println!(
            "cargo::error=a macOS build must link with `-C link-arg=-ObjC` (libwebrtc's Objective-C categories): \
             keep it in RUSTFLAGS or CARGO_ENCODED_RUSTFLAGS when you set them (.cargo/config.toml)"
        );
    }
}
