//! FFmpeg describes HEIC photos made of tiles from 7.1 on; Ubuntu 24.04, which CI and the Linux release build on,
//! ships 6.1, whose headers lack those fields.
fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(ffmpeg_7_1)");
    if std::env::var("DEP_FFMPEG_FFMPEG_7_1").is_ok_and(|v| v == "true") {
        println!("cargo::rustc-cfg=ffmpeg_7_1");
    }
}
