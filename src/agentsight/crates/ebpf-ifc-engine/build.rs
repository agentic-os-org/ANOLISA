// Copies the target-specific full and inode-only CO-RE objects to OUT_DIR.
// Cargo builds need no BPF toolchain unless ACTPLANE_REBUILD_BPF=1 is explicit.
use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let out = PathBuf::from(env::var("OUT_DIR")?);
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH")?;
    let bpf_arch = match target_arch.as_str() {
        "x86_64" => "x86",
        "aarch64" => "arm64",
        _ => return Err(format!("unsupported BPF target architecture: {target_arch}").into()),
    };
    let prebuilt = manifest.join("prebuilt").join(&target_arch);

    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ARCH");
    println!("cargo:rerun-if-env-changed=ACTPLANE_REBUILD_BPF");
    println!("cargo:rerun-if-changed=build.rs");

    if env::var_os("ACTPLANE_REBUILD_BPF").is_some() {
        for file in [
            "process.bpf.c",
            "process.h",
            "taint.h",
            "taint_engine.bpf.h",
            "capability.bpf.h",
            "channel.bpf.h",
            "Makefile",
        ] {
            println!("cargo:rerun-if-changed={}", manifest.join(file).display());
        }
        // Separate output directories prevent a cross build from reusing host
        // objects. Force compilation even if only the selected ARCH changed.
        let output = format!(".output/{target_arch}");
        let status = Command::new("make")
            .arg("-C")
            .arg(&manifest)
            .arg(format!("ARCH={bpf_arch}"))
            .arg(format!("OUTPUT={output}"))
            .args(["-W", "process.bpf.c"])
            .arg(format!("{output}/process.bpf.o"))
            .arg("process-inode-only")
            .status()?;
        if !status.success() {
            return Err(format!("BPF rebuild failed for {target_arch}: {status}").into());
        }
        std::fs::create_dir_all(&prebuilt)?;
        for name in ["process.bpf.o", "process-inode-only.bpf.o"] {
            std::fs::copy(manifest.join(&output).join(name), prebuilt.join(name))?;
        }
    }

    // Missing committed artifacts fail closed; never implicitly invoke make
    // (release builders deliberately lack the BPF headers/toolchain).
    for name in ["process.bpf.o", "process-inode-only.bpf.o"] {
        let source = prebuilt.join(name);
        println!("cargo:rerun-if-changed={}", source.display());
        std::fs::copy(&source, out.join(name))
            .map_err(|error| format!("copy {} -> OUT_DIR: {error}", source.display()))?;
    }
    Ok(())
}
