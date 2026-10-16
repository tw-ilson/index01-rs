fn main() {
    // See csrc/glibc_compat.c: lets the static ONNX Runtime link on glibc < 2.38.
    println!("cargo:rerun-if-changed=csrc/glibc_compat.c");
    let linux_gnu = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu");
    if linux_gnu && std::env::var_os("CARGO_FEATURE_LOCAL").is_some() {
        cc::Build::new().file("csrc/glibc_compat.c").std("gnu11").compile("glibc_compat");
    }
}
