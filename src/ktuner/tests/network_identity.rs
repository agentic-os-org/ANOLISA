//! CLI identity and systemd-sysctl consumption using private procfs fixtures.
#![cfg(target_os = "linux")]

use std::fs;
use std::net::TcpListener;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn why(param: &str) -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_ktuner"))
        .args(["why", param])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{param}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn body() {
    let _listener = TcpListener::bind("127.0.0.1:0").unwrap();
    for proto in ["ipv4", "ipv6"] {
        for iface in ["Br0", "Br0.100"] {
            for key in [
                format!("net/{proto}/conf/{iface}/forwarding"),
                format!("net.{proto}.conf.{iface}.forwarding"),
            ] {
                assert_eq!(why(&key)["current"], "1");
                assert_eq!(
                    why(&key)["param"],
                    format!("net.{proto}.conf.{iface}.forwarding")
                );
            }
        }
        assert_eq!(
            why(&format!("net/{proto}/conf/br0.100/forwarding"))["current"],
            "0"
        );
        assert_eq!(
            why(&format!("net/{proto}/conf/lo/forwarding"))["current"],
            "1"
        );
        fs::remove_file(format!("/proc/sys/net/{proto}/conf/br0.100/forwarding")).unwrap();
        assert_eq!(
            why(&format!("net/{proto}/conf/Br0.100/forwarding"))["current"],
            "1"
        );
    }
    // Built-in rules do not generate per-VLAN recommendations. Seed valid
    // records to exercise cumulative persistence through an ordinary CLI fix.
    let mut entries = serde_json::Map::new();
    for proto in ["ipv4", "ipv6"] {
        let path = format!("/proc/sys/net/{proto}/conf/Br0.100/forwarding");
        fs::write(&path, "0").unwrap();
        let key = if proto == "ipv4" {
            format!("net/{proto}/conf/Br0.100/forwarding")
        } else {
            format!("net.{proto}.conf.Br0.100.forwarding")
        };
        entries.insert(
            key,
            serde_json::json!({"previous": "0", "applied": "1", "path": path}),
        );
    }
    fs::write(
        "/var/lib/ktuner/rollback.json",
        serde_json::json!({"version": 1, "entries": entries}).to_string(),
    )
    .unwrap();
    let fix = Command::new(env!("CARGO_BIN_EXE_ktuner"))
        .args(["fix", "net.core.somaxconn"])
        .output()
        .unwrap();
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let config = "/etc/sysctl.d/99-ktuner.conf";
    let text = fs::read_to_string(config).unwrap();
    for proto in ["ipv4", "ipv6"] {
        assert!(text.contains(&format!("net/{proto}/conf/Br0.100/forwarding = 1")));
    }
    let consumed = Command::new("/usr/lib/systemd/systemd-sysctl")
        .arg(config)
        .output()
        .unwrap();
    assert!(
        consumed.status.success(),
        "{}",
        String::from_utf8_lossy(&consumed.stderr)
    );
    // systemd can exit 0 after ignoring a nonexistent key: inspect the files.
    for proto in ["ipv4", "ipv6"] {
        assert_eq!(
            fs::read_to_string(format!("/proc/sys/net/{proto}/conf/Br0.100/forwarding"))
                .unwrap()
                .trim(),
            "1"
        );
    }
}

#[test]
#[ignore = "requires root, private mount namespaces and systemd-sysctl; only fixture files are written"]
fn network_identity_cli_and_persistence() {
    if std::env::var_os("KTUNER_NETWORK_CHILD").is_some() {
        body();
        return;
    }
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ktuner-network-{}-{nonce}", std::process::id()));
    fs::create_dir_all(dir.join("varlib/ktuner")).unwrap();
    fs::create_dir_all(dir.join("etc")).unwrap();
    for proto in ["ipv4", "ipv6"] {
        for (iface, value) in [
            ("Br0", "1"),
            ("Br0.100", "1"),
            ("br0.100", "0"),
            ("lo", "1"),
        ] {
            fs::create_dir_all(dir.join(proto).join(iface)).unwrap();
            fs::write(dir.join(proto).join(iface).join("forwarding"), value).unwrap();
        }
    }
    fs::write(dir.join("somaxconn"), "1024").unwrap();
    let live = "/proc/sys/net/core/somaxconn";
    let before = fs::read_to_string(live).unwrap();
    let out = Command::new("unshare").args(["--mount", "--propagation", "private", "sh", "-ec",
        "mount --bind \"$1/varlib\" /var/lib; mount --bind \"$1/etc\" /etc/sysctl.d; mount --bind \"$1/somaxconn\" /proc/sys/net/core/somaxconn; mount --bind \"$1/ipv4\" /proc/sys/net/ipv4/conf; mount --bind \"$1/ipv6\" /proc/sys/net/ipv6/conf; KTUNER_NETWORK_CHILD=1 exec \"$2\" --exact network_identity_cli_and_persistence --ignored --nocapture",
        "network-test"]).arg(&dir).arg(std::env::current_exe().unwrap()).output().unwrap();
    assert_eq!(fs::read_to_string(live).unwrap(), before);
    let _ = fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
