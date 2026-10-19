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
    // Oracle's instance processes all run the `oracle` binary, and the engine
    // already matches that name: `is_database_present` and the swappiness,
    // THP, dirty-ratio, dirty-background, NUMA-balancing and read-ahead rules
    // gate on it, and `classify` turns the process into the io-latency
    // workload — without this entry an Oracle-only host reported no database
    // service at all, the hole the mariadbd entry closed for MariaDB.
    ("oracle", "Oracle"),
    ("redis-server", "Redis"),
    // Valkey is the Linux Foundation's fork of Redis 7.2.4; the daemon comm
    // is `valkey-server`. Without this entry a Valkey host reported no
    // cache service at all - the same hole the mariadbd entry closed.
    ("valkey-server", "Valkey"),
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
    ("bookkeeper", "BookKeeper"),
    // The canonical names RUNTIME_SERVICE_MARKERS resolves a JVM or a script
    // to. `read_processes` pushes those names into the same process list this
    // table filters, so without an entry the engine detected the service (and,
    // for hadoop, acted on it) while the report named no service at all — the
    // hole the mariadbd and valkey-server entries closed for their daemons.
    // Appended rather than inserted so an existing host's report keeps its
    // order.
    ("cassandra", "Cassandra"),
    ("hadoop", "Hadoop"),
    ("hbase", "HBase"),
    ("jenkins", "Jenkins"),
    ("logstash", "Logstash"),
    ("solr", "Solr"),
    ("tomcat", "Tomcat"),
    // The runtimes the engine's gates count under their own name. `java` has
    // always had an entry (any JVM reports Java); `node` had none, so a
    // Node.js-only host published `"services": []` while the max_map_count
    // gate sized its mmap limit from `has_process_exact("node")` — the hole
    // the oracle entry closed for the database gates.
    ("node", "Node.js"),
    // Debian and Ubuntu ship the same runtime as `nodejs` (the `node` name was
    // taken by an unrelated package), so the running comm is `nodejs` on those
    // hosts and the entry above — and the gate it mirrors — never matched.
    // `is_generic_runtime` already knew both spellings; the report and the gate
    // now do too.
    ("nodejs", "Node.js"),
];

pub fn detect_services(info: &SystemInfo) -> Vec<&'static str> {
    let mut services: Vec<&'static str> = Vec::new();
    for (proc, label) in KNOWN_SERVICES {
        // Two entries can name one service (`httpd`/`apache2`,
        // `node`/`nodejs`), and a host running both spellings is one host with
        // one service: the report must list it once.
        if info.has_process(proc) && !services.iter().any(|listed| listed == label) {
            services.push(label);
        }
    }
    services
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
            cpu_cores_host: 1,
            numa_nodes: 1,
            memory_total_gb: 1,
            memory_gb_host: 1,
            disks: vec![],
            network: vec![],
            sysctl: SysctlValues {
                swappiness: 60,
                dirty_ratio: 20,
                dirty_background_ratio: 10,
                somaxconn: Some(128),
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
    fn test_detect_services_reports_the_prometheus_server() {
        // The collector filter exists for the exporters named after their
        // target and for the collectors that carry the server's own name
        // (`prometheus-node-exporter`, `prometheus-agent`). The server itself
        // is the workload of a monitoring host — this table lists it as
        // `Prometheus` — and the over-broad prefix clause dropped it, so the
        // entry could never match a live host.
        let info = info_with(&["prometheus"]);
        assert_eq!(detect_services(&info), vec!["Prometheus"]);
        assert!(info.has_process("prometheus"));

        // The collectors that share its name are still not the service.
        let info = info_with(&["prometheus-agent", "prometheus-node-exporter"]);
        assert!(
            detect_services(&info).is_empty(),
            "collectors are not the service: {:?}",
            detect_services(&info)
        );
        assert!(!info.has_process("prometheus"));
    }

    #[test]
    fn test_detect_services_keeps_real_services() {
        let info = info_with(&[
            "postgres",
            "mysqld",
            "redis-server",
            "valkey-server",
            "nginx",
        ]);
        let svcs = detect_services(&info);
        for expected in ["PostgreSQL", "MySQL", "Redis", "Valkey", "Nginx"] {
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
    fn test_detect_services_reports_oracle() {
        // oracle is the one database the engine's gates already count that
        // the report could not name: `is_database_present` and the swappiness,
        // THP, dirty-ratio, dirty-background, NUMA-balancing and read-ahead
        // rules all gate on `has_process("oracle")`, and `classify` turns the
        // process into the io-latency workload — while an Oracle-only host
        // ended up with an empty `services` list. The mariadbd entry and the
        // runtime marker entries closed the same "engine-detected service the
        // report cannot name" hole for their names.
        let info = info_with(&["oracle"]);
        assert_eq!(detect_services(&info), vec!["Oracle"]);
        assert!(info.has_process("oracle"));
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
    fn test_detect_services_bookkeeper_bookie() {
        // The detector names a bookie JVM `bookkeeper`; a Pulsar storage node
        // must report the service it runs.
        let info = info_with(&["bookkeeper"]);
        assert!(detect_services(&info).contains(&"BookKeeper"));
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

    #[test]
    fn test_detect_services_reports_node() {
        // `node` is the runtime the max_map_count gate counts
        // (`has_process_exact("node")`) and the detector's own name list
        // classes as a service rather than a collector, but the report's
        // table had no entry for it: a Node.js-only host published
        // `"services": []` although the engine sized its mmap limit for that
        // workload.
        let info = info_with(&["node"]);
        let svcs = detect_services(&info);
        assert!(svcs.contains(&"Node.js"), "{svcs:?}");
        // node_exporter is a collector of the runtime, not the runtime: the
        // entry must not report Node.js for a metrics-only host.
        let info = info_with(&["node_exporter"]);
        assert!(!detect_services(&info).contains(&"Node.js"));
    }

    #[test]
    fn test_detect_services_reports_the_debian_node_spelling() {
        // Debian/Ubuntu's package is `nodejs`, so that is the comm a Node.js
        // host has there — `is_generic_runtime` already counted both spellings
        // while this table and the max_map_count gate named only `node`.
        let info = info_with(&["nodejs"]);
        let svcs = detect_services(&info);
        assert!(svcs.contains(&"Node.js"), "{svcs:?}");

        // A host that runs both spellings has one service, not two entries.
        let info = info_with(&["node", "nodejs"]);
        let svcs = detect_services(&info);
        assert_eq!(
            svcs.iter().filter(|label| **label == "Node.js").count(),
            1,
            "two spellings, one service: {svcs:?}"
        );

        // The same rule for the Apache pair, which has carried two entries for
        // longer: one service however many of its spellings are running.
        let info = info_with(&["httpd", "apache2"]);
        let svcs = detect_services(&info);
        assert_eq!(
            svcs.iter().filter(|label| **label == "Apache").count(),
            1,
            "two spellings, one service: {svcs:?}"
        );
    }
}
