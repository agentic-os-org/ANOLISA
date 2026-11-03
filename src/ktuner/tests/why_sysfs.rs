use std::path::PathBuf;
use std::process::{Command, Output};

fn ktuner(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ktuner"))
        .args(arguments)
        .output()
        .expect("run read-only ktuner command")
}

#[test]
fn why_reads_available_sysfs_parameters_without_recommendations() {
    let check = ktuner(&["check"]);
    assert!(matches!(check.status.code(), Some(0 | 1)));
    let evaluation: serde_json::Value = serde_json::from_slice(&check.stdout).unwrap();
    let recommendations = evaluation["recommendations"].as_array().unwrap();
    let mut parameters = vec![
        (
            "transparent_hugepage/enabled".to_string(),
            PathBuf::from("/sys/kernel/mm/transparent_hugepage/enabled"),
        ),
        (
            "transparent_hugepage/defrag".to_string(),
            PathBuf::from("/sys/kernel/mm/transparent_hugepage/defrag"),
        ),
    ];
    if let Ok(disks) = std::fs::read_dir("/sys/block") {
        for disk in disks.flatten() {
            let Some(name) = disk.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let path = disk.path().join("queue/scheduler");
            if path.is_file() {
                parameters.push((format!("block/{name}/scheduler"), path));
                break;
            }
        }
    }

    let mut checked = 0;
    for (param, path) in parameters {
        let Ok(current) = std::fs::read_to_string(&path) else {
            eprintln!("skip {param}: sysfs parameter is unavailable or unreadable");
            continue;
        };
        if recommendations.iter().any(|rec| rec["param"] == param) {
            eprintln!("skip {param}: this host has a recommendation rather than a fallback");
            continue;
        }

        let result = ktuner(&["why", &param]);
        assert_eq!(
            result.status.code(),
            Some(0),
            "{param}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stderr.is_empty());
        let output: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        // The parameter files here are option lists ("always [madvise]
        // never"): the reported current value is the bracketed active
        // option, not the whole rendering.
        let raw = current.trim();
        let active = raw
            .split_whitespace()
            .find_map(|token| token.strip_prefix('[').and_then(|t| t.strip_suffix(']')))
            .unwrap_or(raw);
        assert_eq!(
            output,
            serde_json::json!({ "param": param, "current": active, "status": "optimal" })
        );
        checked += 1;
    }
    eprintln!("validated {checked} real sysfs fallback queries");
}

#[test]
fn why_keeps_real_sysctl_aliases_and_case_compatibility() {
    let Ok(current) = std::fs::read_to_string("/proc/sys/kernel/ostype") else {
        eprintln!("skip: kernel.ostype is unavailable or unreadable on this host");
        return;
    };
    for param in [
        "kernel.ostype",
        "kernel/ostype",
        "KERNEL.OSTYPE",
        "KERNEL/OSTYPE",
    ] {
        let result = ktuner(&["why", param]);
        assert_eq!(result.status.code(), Some(0));
        assert!(result.stderr.is_empty());
        let output: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(
            output,
            serde_json::json!({ "param": "kernel.ostype", "current": current.trim(), "status": "optimal" })
        );
    }
}
