//! Reserve the stack required by the bundled RTK command parser on Windows.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bin=tokenless=/STACK:8388608");
    }
}
