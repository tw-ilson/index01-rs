//! Direct PC ↔ Index 01 ring communication.
//!
//! Ported from the decompiled Haversine library ([`haversine`]):
//! - [`protocol`]: Telesto wire format, addresses, advertisement parsing.
//! - [`collection`]: collection blob parser and Rice audio decoder.
//! - [`appdata`]: the user record (application data) and its fingerprint.
//!
//! New code written for this crate:
//! - [`telesto`]: the one-op-at-a-time request runner, generic over a [`telesto::Link`].
//! - [`client`]: the btleplug/BlueZ implementation of that link plus high-level reads.
//! - [`wav`]: WAV output with the phone app's DC-bias removal.
//! - [`transcribe`]: pluggable speech-to-text backends (OpenAI-compatible server or command).
//! - the `index01` CLI (`src/main.rs`).

pub mod haversine;

pub mod client;
#[cfg(all(feature = "local", target_os = "linux", target_env = "gnu"))]
mod glibc_compat;
pub mod paste;
pub mod telesto;
pub mod transcribe;
pub mod wav;

pub use haversine::{appdata, collection, protocol};
