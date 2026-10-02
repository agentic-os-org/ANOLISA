use crate::detect::SystemInfo;

const KNOWN_SERVICES: &[(&str, &str)] = &[
    ("postgres", "PostgreSQL"),
    ("mysqld", "MySQL"),
    ("mongod", "MongoDB"),
    ("clickhouse", "ClickHouse"),
    ("redis-server", "Redis"),
    ("memcached", "Memcached"),
    ("elasticsearch", "Elasticsearch"),
    ("opensearch", "OpenSearch"),
    ("nginx", "Nginx"),
    ("httpd", "Apache"),
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
                tcp_fastopen: 1,
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
}
