// Makes the prebuilt eBPF CO-RE objects available to lib.rs (include_bytes! from
// OUT_DIR). Two variants are maintained:
//   - process.bpf.o:            full build with bpf_d_path (kernel 7.1.x+)
//   - process-inode-only.bpf.o: compiled with -DINODE_GUARD_ONLY, no bpf_d_path
//                                 (kernel 5.10/6.6 where verifier rejects it)
//
// By default this just copies the committed prebuilt/*.bpf.o files, so
// `cargo build` / `cargo install` needs NO clang/llvm/libbpf and no submodules.
//
// Set ACTPLANE_REBUILD_BPF=1 to rebuild both from the kernel C via the Makefile.
use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let prebuilt_full = manifest.join("prebuilt/process.bpf.o");
    let prebuilt_inode = manifest.join("prebuilt/process-inode-only.bpf.o");
    let built_full = manifest.join(".output/process.bpf.o");
    let built_inode = manifest.join(".output/process-inode-only.bpf.o");

    println!("cargo:rerun-if-env-changed=ACTPLANE_REBUILD_BPF");
    println!("cargo:rerun-if-changed={}", prebuilt_full.display());
    println!("cargo:rerun-if-changed={}", prebuilt_inode.display());

    let rebuild = env::var_os("ACTPLANE_REBUILD_BPF").is_some();

    if rebuild || !prebuilt_full.exists() {
        for f in [
            "process.bpf.c",
            "process.h",
            "taint.h",
            "taint_engine.bpf.h",
            "capability.bpf.h",
            "channel.bpf.h",
            "Makefile",
        ] {
            println!("cargo:rerun-if-changed={}", manifest.join(f).display());
        }
        let status = Command::new("make")
            .arg("-C")
            .arg(&manifest)
            .arg("process")
            .status()
            .expect("run make -C bpf process (ACTPLANE_REBUILD_BPF)");
        assert!(status.success(), "make -C bpf process failed");
        std::fs::create_dir_all(manifest.join("prebuilt")).ok();
        std::fs::copy(&built_full, &prebuilt_full)
            .unwrap_or_else(|e| panic!("copy {} -> prebuilt: {e}", built_full.display()));
        // Also rebuild the inode-only variant if the Makefile target exists.
        let _ = Command::new("make")
            .arg("-C")
            .arg(&manifest)
            .arg("process-inode-only")
            .status();
        if built_inode.exists() {
            std::fs::copy(&built_inode, &prebuilt_inode).ok();
        }
    }

    // Full variant (always required).
    let src_full = if prebuilt_full.exists() {
        &prebuilt_full
    } else {
        &built_full
    };
    std::fs::copy(src_full, out.join("process.bpf.o"))
        .unwrap_or_else(|e| panic!("copy {} -> OUT_DIR: {e}", src_full.display()));

    // Inode-only variant: fall back to the full variant if no separate build exists
    // (ensures compilation succeeds even without the inode-only prebuilt).
    let src_inode = if prebuilt_inode.exists() {
        prebuilt_inode
    } else if built_inode.exists() {
        built_inode
    } else {
        src_full.clone()
    };
    std::fs::copy(&src_inode, out.join("process-inode-only.bpf.o"))
        .unwrap_or_else(|e| panic!("copy {} -> OUT_DIR: {e}", src_inode.display()));
}
