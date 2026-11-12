//! Thin facade exposed to Flutter via flutter_rust_bridge.
//!
//! Everything under [`api`] is turned into Dart by `flutter_rust_bridge_codegen generate`
//! (run in `app/`). Panics in these functions are caught by the bridge and surface as Dart
//! exceptions; they never take the app down.

#[cfg(target_os = "android")]
mod android;
pub mod api;
mod frb_generated;
pub mod media_controls;
