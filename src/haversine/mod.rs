//! Ports of the closed Haversine library, nothing else.
//!
//! Every item here replicates behaviour recovered from the decompiled library: the native
//! `libhaversinesatellitelibrary.so` / `libppcommon.so` (Ghidra via `rea`, `decomp/*.c`) and
//! its Java layer (`vendor/java_src/`). Each item cites its source address or class. Pure
//! code, no I/O. Deliberate differences from the library are marked `DEVIATION:`.
//!
//! Everything outside this module (BLE transport, op runner, WAV output, CLI) is new code.

pub mod appdata;
pub mod collection;
pub mod controller;
pub mod protocol;
