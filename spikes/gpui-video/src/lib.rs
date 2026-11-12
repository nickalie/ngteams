//! M0 spike 2 (Tasks 15–16): GPUI on niri/Wayland, then video rendering.
//!
//! The library holds what the `gpui-video` binary and its tests share: the synthetic video source ([`frames`]), the
//! video grid view and its source threads ([`grid`]), the benchmark's statistics and parsers ([`bench`]), and the
//! private headless weston + nested niri the tests and the benchmark run in ([`sandbox`]).
#![cfg(target_os = "linux")]

pub mod bench;
pub mod frames;
pub mod grid;
pub mod sandbox;
