use std::env;
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use libbpf_cargo::SkeletonBuilder;

/// Read the Ring Buffer size (in MiB) from the `AGENTSIGHT_RING_BUFFER_MB`
/// environment variable and return the corresponding clang `-D` flag.
///
/// The BPF ring buffer `max_entries` must be a power-of-two multiple of the
/// page size.  We accept any positive integer of MiB here and let the kernel
/// validate at load time; common values are 8, 16, 32, 64.
///
/// Default: 32 MiB (matches `common.h` fallback).
#[cfg(target_os = "linux")]
fn ring_buffer_clang_define() -> String {
    let mb = env::var("AGENTSIGHT_RING_BUFFER_MB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&v| v > 0);
    match mb {
        Some(v) => {
            let bytes = v * 1024 * 1024;
            println!("cargo:warning=AGENTSIGHT_RING_BUFFER_MB={v} ({bytes} bytes)");
            format!("-DRING_BUFFER_SIZE={bytes}")
        }
        None => String::new(),
    }
}

#[cfg(target_os = "linux")]
fn generate_skeleton(out: &mut PathBuf, name: &str, clang_define: &str) {
    let c_path = format!("src/bpf/{name}.bpf.c");
    let rs_name = format!("{name}.skel.rs");
    out.push(&rs_name);

    let mut builder = SkeletonBuilder::new();
    builder.source(&c_path);
    if !clang_define.is_empty() {
        builder.clang_args([clang_define]);
    }
    builder.build_and_generate(&out).unwrap();

    out.pop();
    println!("cargo:rerun-if-changed={c_path}");
}

#[cfg(target_os = "linux")]
fn generate_header(out: &mut PathBuf, name: &str) {
    let header_path = format!("src/bpf/{name}.h");
    let rs_name = format!("{name}.rs");

    out.push(&rs_name);
    let bindings = bindgen::Builder::default()
        .header(&header_path)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .unwrap();
    bindings.write_to_file(&out).unwrap();
    out.pop();

    println!("cargo:rerun-if-changed={header_path}");
}

#[cfg(target_os = "linux")]
fn generate_ebpf_artifacts(out: &mut PathBuf) {
    let ring_define = ring_buffer_clang_define();
    println!("cargo:rerun-if-env-changed=AGENTSIGHT_RING_BUFFER_MB");
    println!("cargo:rerun-if-changed=src/bpf/common.h");

    generate_skeleton(out, "sslsniff", &ring_define);
    generate_header(out, "sslsniff");

    generate_skeleton(out, "proctrace", &ring_define);
    generate_header(out, "proctrace");

    generate_skeleton(out, "procmon", &ring_define);
    generate_header(out, "procmon");

    generate_skeleton(out, "filewatch", &ring_define);
    generate_header(out, "filewatch");

    generate_skeleton(out, "filewrite", &ring_define);
    generate_header(out, "filewrite");

    generate_skeleton(out, "udpdns", &ring_define);
    generate_header(out, "udpdns");

    generate_skeleton(out, "tcpsniff", &ring_define);
}

#[cfg(target_os = "linux")]
fn extract_header_decl_names(header: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for l in header.lines() {
        let first_is_ident = l
            .chars()
            .next()
            .map(|c| c.is_ascii_alphabetic() || c == '_')
            .unwrap_or(false);
        if !first_is_ident || l.starts_with("typedef") {
            continue;
        }
        let bytes = l.as_bytes();
        let mut i = 0;
        while i + "agentsight_".len() < bytes.len() {
            if l[i..].starts_with("agentsight_") {
                let mut j = i + "agentsight_".len();
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'(' {
                    out.insert(l[i..j].to_string());
                    break;
                }
                i = j;
            } else {
                i += 1;
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn extract_ffi_export_names(src: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for l in src.lines() {
        let t = l.trim_start();
        let after_fn = if let Some(rest) = t.strip_prefix("pub extern \"C\" fn ") {
            rest
        } else if let Some(rest) = t.strip_prefix("pub unsafe extern \"C\" fn ") {
            rest
        } else {
            continue;
        };
        if let Some(name) = after_fn.split('(').next() {
            let name = name.trim();
            if name.starts_with("agentsight_") {
                out.insert(name.to_string());
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn check_ffi_header_drift(crate_dir: &str, header_path: &std::path::Path) {
    let ffi_path = PathBuf::from(crate_dir).join("src/ffi.rs");
    let ffi_src = std::fs::read_to_string(&ffi_path).unwrap_or_else(|e| {
        panic!(
            "FFI drift guard: cannot read {}: {e}. If you renamed or moved ffi.rs, \
             update build.rs::check_ffi_header_drift.",
            ffi_path.display()
        )
    });
    let header = std::fs::read_to_string(header_path).unwrap_or_else(|e| {
        panic!(
            "FFI drift guard: cannot read generated header {}: {e}. cbindgen \
             should have just written it; check earlier cbindgen errors.",
            header_path.display()
        )
    });

    let ffi_names = extract_ffi_export_names(&ffi_src);
    let header_names = extract_header_decl_names(&header);

    let marker_count = ffi_src.matches("#[unsafe(no_mangle)]").count();
    assert_eq!(
        marker_count,
        ffi_names.len(),
        "FFI drift guard: {} `#[unsafe(no_mangle)]` markers in src/ffi.rs but \
         extract_ffi_export_names found {} `pub [unsafe] extern \"C\" fn agentsight_*` \
         declarations. The extractor likely needs updating (build.rs).",
        marker_count,
        ffi_names.len()
    );

    let missing_in_header: Vec<&String> = ffi_names.difference(&header_names).collect();
    let stale_in_header: Vec<&String> = header_names.difference(&ffi_names).collect();

    if !missing_in_header.is_empty() || !stale_in_header.is_empty() {
        panic!(
            "FFI header drift detected — update the `after_includes` block in cbindgen.toml.\n\
             missing in header (declared in src/ffi.rs but absent from cbindgen.toml): {missing_in_header:?}\n\
             stale in header   (declared in cbindgen.toml but absent from src/ffi.rs):  {stale_in_header:?}\n\
             NOTE: this guard checks NAMES only; signature drift (return type, \
             parameter types/order) is NOT detected — verify by hand.",
        );
    }
}

#[cfg(target_os = "linux")]
fn generate_c_header(crate_dir: &str) {
    let header_path = PathBuf::from(crate_dir)
        .join("include")
        .join("agentsight.h");
    std::fs::create_dir_all(header_path.parent().unwrap())
        .expect("Failed to create include/ directory");
    cbindgen::Builder::new()
        .with_crate(crate_dir)
        .with_config(
            cbindgen::Config::from_file(PathBuf::from(crate_dir).join("cbindgen.toml"))
                .expect("Failed to read cbindgen.toml"),
        )
        .with_parse_exclude(&["skill_metrics".to_string()])
        .generate()
        .expect("cbindgen failed to generate C header")
        .write_to_file(&header_path);
    println!("cargo:rerun-if-changed=src/ffi.rs");
    println!("cargo:rerun-if-changed=cbindgen.toml");

    check_ffi_header_drift(crate_dir, &header_path);
}

fn main() {
    #[cfg(target_os = "linux")]
    {
        let mut out =
            PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR must be set in build script"));
        generate_ebpf_artifacts(&mut out);
    }

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set");
    let frontend_dist = PathBuf::from(&manifest_dir).join("frontend-dist");
    if !frontend_dist.exists() {
        std::fs::create_dir_all(&frontend_dist).expect("Failed to create frontend-dist directory");
    }
    println!("cargo:rerun-if-changed=frontend-dist");
    if let Ok(entries) = std::fs::read_dir(&frontend_dist) {
        for entry in entries.flatten() {
            println!("cargo:rerun-if-changed={}", entry.path().display());
        }
    }

    #[cfg(target_os = "linux")]
    {
        generate_c_header(&manifest_dir);
    }
}
