use super::{shell_output_facts, ShellOutputFacts};

#[test]
fn command_output_facts_ordinary_metrics_require_complete_static_sources() {
    for command in [
        "uptime",
        "free -h",
        "nproc",
        "df -h /",
        "ps -eo pid,ppid,user,%cpu,%mem,rss,etime,comm --sort=-%cpu | head -n 10",
        "ps -e -o pid,ppid,user,pcpu,pmem,rss,etime,comm --sort=-pcpu",
        "printf '---\\n'; uptime; free -h; nproc; df -h /",
        "command -p uptime",
        "timeout 5 free -h",
        "env LC_ALL=C free -h",
        "sudo -n -usysadmin uptime",
        "sudo -n -u sysadmin uptime",
        "sudo -n env -i uptime",
    ] {
        assert_eq!(
            shell_output_facts(command),
            ShellOutputFacts::default(),
            "{command}"
        );
    }
    for command in [
        "",
        "head -n 1",
        "custom-inspect",
        "uptime; head -n 1 unknown-file",
        "df \"$TARGET\"",
        "printf \"$(printenv)\"",
        "printf \"$SECRET\"",
        "free -h > output",
        "env --split-string='printenv'",
        "bash -c 'uptime'",
        "ps -eo comm,unknown",
        "echo 'printenv'",
        "ps -eo comm --sort=unknown",
        "ps -eo comm --sort=cmd",
        "ps -o comm=TEST_SYNTHETIC_HEADER",
        "xargs -a /tmp/synthetic-input -n 1 df -h",
        "xargs -a /tmp/synthetic-input echo '---'",
        "/usr/bin/time -f 'synthetic-secret' uptime",
        "time -v env TEST_SECRET=SYNTHETIC uptime",
        "sudo -i uptime",
        "sudo --login uptime",
        "sudo -s uptime",
        "sudo --shell uptime",
        "sudo -n -i uptime",
        "sudo -iu root uptime",
        "sudo -u root --login uptime",
        "timeout --verbose 5 free -h",
    ] {
        assert!(shell_output_facts(command).unknown, "{command}");
    }
}

#[test]
fn command_output_facts_environment_sources_survive_compound_and_launchers() {
    for command in [
        "printenv",
        "printenv HOME TEST_SECRET",
        "/usr/bin/printenv -0 TEST_SECRET",
        "printenv -- --help",
        "env",
        "env -i TEST_SECRET=synthetic",
        "env -u HOME",
        "env --null",
        "uptime; printenv",
        "printenv | head -n 1",
        "printenv > output",
        "sudo -n env TEST_SECRET=synthetic printenv",
        "timeout -s TERM 5 /usr/bin/printenv",
        "command -p printenv",
        "sudo env",
        "timeout 5 env",
        "command env",
        "printenv -0 -- --help",
    ] {
        assert!(shell_output_facts(command).environment, "{command}");
    }
    for command in [
        "echo printenv",
        "printenv --help",
        "printenv --version",
        "env pwd",
        "command -v printenv",
        "sudo -l printenv",
        "printf '%s' printenv",
    ] {
        assert!(!shell_output_facts(command).environment, "{command}");
    }
}

#[test]
fn command_output_facts_distinguish_process_names_from_full_arguments() {
    for command in [
        "ps aux",
        "ps -ef",
        "ps -eo pid,args",
        "ps -e -o command",
        "ps -p 1 -o args",
        "ps -eo args --sort pid",
        "ps -o args --sort=cmd",
        "ps -u root -o args",
        "ps -o args=CMD",
        "ps -o args:80",
        "ps -o 'pid args=CMD'",
    ] {
        assert!(shell_output_facts(command).arguments, "{command}");
    }
    for command in ["ps -eo comm", "ps -e -o pid,comm", "echo 'ps aux'"] {
        assert!(!shell_output_facts(command).arguments, "{command}");
    }
    assert!(shell_output_facts("cat ~/.ssh/id_rsa").sensitive);
    let combined = shell_output_facts("uptime; printenv; ps aux; cat ~/.ssh/id_rsa");
    assert!(combined.environment && combined.arguments && combined.sensitive);
    assert!(!shell_output_facts("sudo -S pwd").environment);
    let wrapped = shell_output_facts("xargs -a /tmp/.env echo '---'");
    assert!(wrapped.sensitive && wrapped.unknown);
}

#[test]
fn command_output_facts_attached_eo_fields_keep_argument_disclosure() {
    for command in ["ps -eoargs", "ps -eocmd", "ps -eoargs=CMD", "ps -eoargs:80"] {
        assert!(shell_output_facts(command).arguments, "{command}");
    }
    assert!(!shell_output_facts("ps -eopid,comm").arguments);
}

#[test]
fn command_output_facts_ao_fields_keep_argument_disclosure() {
    for command in ["ps -Ao pid,command", "ps -Aopid,command"] {
        assert!(shell_output_facts(command).arguments, "{command}");
    }
    for command in ["ps -Ao pid,comm", "ps -Aopid,comm"] {
        assert!(!shell_output_facts(command).arguments, "{command}");
        assert!(!shell_output_facts(command).unknown, "{command}");
    }
}

#[test]
fn command_output_facts_env_attached_values_keep_environment_disclosure() {
    for command in [
        "env -uHOME",
        "env -u HOME",
        "env -C/tmp",
        "env -C /tmp",
        "env --unset=HOME",
        "env --chdir=/tmp",
        "sudo -n env -uHOME",
        "command env -uHOME",
        "timeout 5 env -C/tmp",
    ] {
        assert!(shell_output_facts(command).environment, "{command}");
    }
    for command in [
        "env -uHOME uptime",
        "env -u HOME uptime",
        "env -C/tmp pwd",
        "env -C /tmp pwd",
        "sudo -n env -uHOME uptime",
        "command env -C/tmp pwd",
        "env --help",
    ] {
        assert!(!shell_output_facts(command).environment, "{command}");
    }
}
