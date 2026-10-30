//! Reserves the main-thread stack the embedded RTK command line needs on Windows.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Clap and RTK's full command graph can exceed the default 1 MiB Windows
    // main-thread stack. RTK's own build script only covers its standalone
    // binary, and it checks the host rather than the target. `/STACK` is an
    // MSVC linker option and MSVC is the only Windows release target, so GNU
    // builds keep their linker's default stack.
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        println!("cargo:rustc-link-arg-bin=tokenless=/STACK:8388608");
    }
}
