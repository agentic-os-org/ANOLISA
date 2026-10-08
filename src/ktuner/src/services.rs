use crate::detect::SystemInfo;

const KNOWN_SERVICES: &[(&str, &str)] = &[
    ("postgres", "PostgreSQL"),
    ("mysqld", "MySQL"),
    // MariaDB names its daemon `mariadbd` (10.4+; distro packages also keep
    // a mysqld alias/symlink, but the running comm is mariadbd). Without
    // this entry a MariaDB host reported no database service at all.
    ("mariadbd", "MariaDB"),
    ("mongod", "MongoDB"),
    ("clickhouse", "ClickHouse"),
    ("redis-server", "Redis"),
    ("memcached", "Memcached"),
    ("elasticsearch", "Elasticsearch"),
    ("opensearch", "OpenSearch"),
    ("nginx", "Nginx"),
    ("httpd", "Apache"),
    // Debian/Ubuntu/SUSE name the Apache binary `apache2`; the same server
    // as RHEL's httpd, and absent from the service list without this entry.
    ("apache2", "Apache"),
    ("envoy", "Envoy"),
    ("haproxy", "HAProxy"),
    ("caddy", "Caddy"),
    ("kafka", "Kafka"),
    ("flink", "Flink"),
    ("spark", "Spark"),
    ("etcd", "etcd"),
    ("java", "Java"),
    ("kubelet", "K8s"),
    ("rabbitmq", "RabbitMQ"),
    ("zookeeper", "ZooKeeper"),
    ("consul", "Consul"),
    ("prometheus", "Prometheus"),
    ("grafana", "Grafana"),
    ("dockerd", "Docker"),
    ("containerd", "containerd"),
    ("coredns", "CoreDNS"),
    ("node_export", "NodeExporter"),
    ("tidb-server", "TiDB"),
    ("tikv-server", "TiKV"),
    ("minio", "MinIO"),
    ("pulsar", "Pulsar"),
];

pub fn detect_services(info: &SystemInfo) -> Vec<&'static str> {
    KNOWN_SERVICES
        .iter()
        .filter(|(proc, _)| info.has_process(proc))
        .map(|(_, label)| *label)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::*;

    fn info_with(procs: &[&str]) -> SystemInfo {
        SystemInfo {
            kernel_version: String::new(),
            os_distro: String::new(),
            cpu_model: String::new(),
            cpu_cores: 1,
            numa_nodes: 1,
            memory_total_gb: 1,
            disks: vec![],
            network: vec![],
            sysctl: SysctlValues {
                swappiness: 60,
                dirty_ratio: 20,
                dirty_background_ratio: 10,
                somaxconn: 128,
                thp_enabled: "always".into(),
            },
            processes: procs
                .iter()
                .map(|n| ProcessInfo {
                    name: n.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn test_detect_services_finds_known() {
        let info = info_with(&["nginx", "postgres", "unknown"]);
        let svcs = detect_services(&info);
        assert!(svcs.contains(&"Nginx"));
        assert!(svcs.contains(&"PostgreSQL"));
        assert!(!svcs.contains(&"unknown"));
    }

    #[test]
    fn test_detect_services_empty() {
        let info = info_with(&[]);
        assert!(detect_services(&info).is_empty());
    }

    #[test]
    fn test_detect_services_ignores_exporters() {
        // Prometheus-style collectors share a prefix with the service they
        // monitor, so a substring match reports PostgreSQL, MySQL and friends as
        // running on a host that only scrapes metrics. read_processes drops them,
        // and has_process must not reintroduce them from a raw name either.
        let info = info_with(&[
            "postgres_exporter",
            "mysqld_exporter",
            "redis_exporter",
            "node_exporter",
        ]);
        assert!(
            detect_services(&info).is_empty(),
            "exporters are not the monitored service: {:?}",
            detect_services(&info)
        );
        assert!(!info.has_process("postgres"));
        assert!(!info.has_process("mysqld"));
        assert!(!info.has_process("node"));
    }

    #[test]
    fn test_detect_services_keeps_real_services() {
        let info = info_with(&["postgres", "mysqld", "redis-server", "nginx"]);
        let svcs = detect_services(&info);
        for expected in ["PostgreSQL", "MySQL", "Redis", "Nginx"] {
            assert!(svcs.contains(&expected), "missing {expected}: {svcs:?}");
        }
    }

    #[test]
    fn test_detect_services_matches_truncated_comm_names() {
        // /proc/<pid>/comm is 15 bytes, so nginx workers and postgres
        // background workers are truncated to a "<service>: <role>" prefix and
        // must still be recognized as the service.
        let info = info_with(&["nginx: worker p", "postgres: writer"]);
        let svcs = detect_services(&info);
        assert!(svcs.contains(&"Nginx"), "{svcs:?}");
        assert!(svcs.contains(&"PostgreSQL"), "{svcs:?}");
    }

    #[test]
    fn test_detect_services_mariadb_under_its_own_daemon_name() {
        // MariaDB 10.4+ runs as `mariadbd`; a MariaDB host must report a
        // database service (and its role-suffixed worker comms count too).
        let info = info_with(&["mariadbd"]);
        let svcs = detect_services(&info);
        assert!(svcs.contains(&"MariaDB"), "{svcs:?}");
        let info = info_with(&["mariadbd: writer"]);
        assert!(detect_services(&info).contains(&"MariaDB"));
        // The client tool is not the server.
        let info = info_with(&["mariadb-dump"]);
        assert!(!detect_services(&info).contains(&"MariaDB"));
    }

    #[test]
    fn test_detect_services_apache_under_its_debian_name() {
        // Debian/Ubuntu/SUSE name the Apache binary `apache2` (RHEL: httpd);
        // a Debian Apache host must report Apache under either name, and the
        // workers' role-suffixed comms count too.
        let info = info_with(&["apache2"]);
        let svcs = detect_services(&info);
        assert!(svcs.contains(&"Apache"), "{svcs:?}");
        let info = info_with(&["httpd"]);
        assert!(detect_services(&info).contains(&"Apache"));
        let info = info_with(&["apache2: worker p"]);
        assert!(detect_services(&info).contains(&"Apache"));
        // The server, not a client tool with a shared prefix.
        let info = info_with(&["apache2ctl"]);
        assert!(!detect_services(&info).contains(&"Apache"));
    }
}
