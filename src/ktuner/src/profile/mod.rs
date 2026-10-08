use crate::detect::SystemInfo;

#[derive(Debug, Clone, PartialEq)]
pub enum WorkloadType {
    MemoryIntensive,
    IoThroughput,
    IoLatency,
    NetworkIntensive,
    Mixed,
}

impl std::fmt::Display for WorkloadType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkloadType::MemoryIntensive => write!(f, "memory-intensive"),
            WorkloadType::IoThroughput => write!(f, "io-throughput"),
            WorkloadType::IoLatency => write!(f, "io-latency"),
            WorkloadType::NetworkIntensive => write!(f, "network-intensive"),
            WorkloadType::Mixed => write!(f, "mixed"),
        }
    }
}

impl WorkloadType {
    pub fn description(&self) -> &str {
        match self {
            WorkloadType::MemoryIntensive => "内存密集型（数据库、缓存、大数据）",
            WorkloadType::IoThroughput => "高吞吐 IO（批量处理、ETL、日志）",
            WorkloadType::IoLatency => "低延迟 IO（OLTP 数据库、KV 存储）",
            WorkloadType::NetworkIntensive => "网络密集型（Web 服务、RPC、代理）",
            WorkloadType::Mixed => "混合型负载",
        }
    }
}

pub fn classify(info: &SystemInfo) -> WorkloadType {
    // Process-based heuristics
    let has_db = info.has_process("postgres")
        || info.has_process("mysqld")
        // MariaDB 10.4+ runs as mariadbd; the same OLTP workload as MySQL.
        || info.has_process("mariadbd")
        // TiDB is an OLTP database and TiKV its distributed KV store — both
        // are services the engine already detects (KNOWN_SERVICES); the
        // io-latency profile is defined as "OLTP databases, KV stores".
        || info.has_process("tidb-server")
        || info.has_process("tikv-server")
        || info.has_process("mongod")
        || info.has_process("clickhouse");
    let has_cache = info.has_process("redis-server")
        || info.has_process("memcached")
        || info.has_process("etcd")
        // ZooKeeper and Consul are the same in-memory coordination-KV class
        // as etcd; the runtime marker table already resolves
        // org.apache.zookeeper to the "zookeeper" service.
        || info.has_process("zookeeper")
        || info.has_process("consul");
    let has_web = info.has_process("nginx")
        || info.has_process("httpd")
        // Debian/Ubuntu/SUSE run the Apache binary as `apache2`; RHEL and
        // the httpd name cover the same server. Without this branch every
        // Debian Apache host classified as mixed, not network-intensive.
        || info.has_process("apache2")
        || info.has_process("envoy")
        || info.has_process("haproxy")
        || info.has_process("caddy");
    let has_java = info.has_process("java");
    let has_search = info.has_process("elasticsearch") || info.has_process("opensearch");
    let has_streaming = info.has_process("kafka")
        || info.has_process("flink")
        || info.has_process("spark")
        // Pulsar is the same event-streaming class as Kafka; the runtime
        // detector already resolves org.apache.pulsar broker cmdlines to
        // the "pulsar" service.
        || info.has_process("pulsar");

    if has_db {
        return WorkloadType::IoLatency;
    }

    if has_cache {
        return WorkloadType::MemoryIntensive;
    }

    if has_search {
        return WorkloadType::MemoryIntensive;
    }

    if has_web && !has_db {
        return WorkloadType::NetworkIntensive;
    }

    if has_streaming {
        return WorkloadType::IoThroughput;
    }

    if has_java {
        return WorkloadType::Mixed;
    }

    WorkloadType::Mixed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::*;

    fn make_info(processes: Vec<&str>) -> SystemInfo {
        SystemInfo {
            kernel_version: String::new(),
            os_distro: String::new(),
            cpu_model: String::new(),
            cpu_cores: 8,
            numa_nodes: 1,
            memory_total_gb: 64,
            disks: vec![],
            network: vec![],
            sysctl: SysctlValues {
                swappiness: 60,
                dirty_ratio: 20,
                dirty_background_ratio: 10,
                somaxconn: 128,
                thp_enabled: "always".to_string(),
            },
            processes: processes
                .iter()
                .map(|name| ProcessInfo {
                    name: name.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn test_classify_database() {
        assert_eq!(
            classify(&make_info(vec!["postgres"])),
            WorkloadType::IoLatency
        );
        assert_eq!(
            classify(&make_info(vec!["mysqld"])),
            WorkloadType::IoLatency
        );
        assert_eq!(
            classify(&make_info(vec!["mongod"])),
            WorkloadType::IoLatency
        );
    }

    #[test]
    fn test_classify_cache() {
        assert_eq!(
            classify(&make_info(vec!["redis-server"])),
            WorkloadType::MemoryIntensive
        );
        assert_eq!(
            classify(&make_info(vec!["memcached"])),
            WorkloadType::MemoryIntensive
        );
    }

    #[test]
    fn test_classify_web() {
        assert_eq!(
            classify(&make_info(vec!["nginx"])),
            WorkloadType::NetworkIntensive
        );
        assert_eq!(
            classify(&make_info(vec!["envoy"])),
            WorkloadType::NetworkIntensive
        );
    }

    #[test]
    fn test_classify_apache_under_its_debian_name() {
        // Debian/Ubuntu/SUSE run the Apache binary as `apache2`; the RHEL
        // name `httpd` is the same server. A pure Apache host is a web
        // workload under either name — not mixed.
        assert_eq!(
            classify(&make_info(vec!["apache2"])),
            WorkloadType::NetworkIntensive
        );
        assert_eq!(
            classify(&make_info(vec!["httpd"])),
            WorkloadType::NetworkIntensive
        );
        // apache2 wins over nothing: a db still classifies first.
        assert_eq!(
            classify(&make_info(vec!["apache2", "postgres"])),
            WorkloadType::IoLatency
        );
    }

    #[test]
    fn test_classify_mariadb() {
        // MariaDB's daemon comm is `mariadbd`; a MariaDB-only host is an
        // OLTP database workload, not mixed.
        assert_eq!(
            classify(&make_info(vec!["mariadbd"])),
            WorkloadType::IoLatency
        );
        // Role-suffixed worker comms classify the same.
        assert_eq!(
            classify(&make_info(vec!["mariadbd: writer"])),
            WorkloadType::IoLatency
        );
    }

    #[test]
    fn test_classify_tidb() {
        // TiDB is an OLTP database and TiKV its distributed KV store — both
        // are services the detector already knows (KNOWN_SERVICES). classify
        // must route a TiDB host to the io-latency profile, not mixed.
        assert_eq!(
            classify(&make_info(vec!["tidb-server"])),
            WorkloadType::IoLatency
        );
        assert_eq!(
            classify(&make_info(vec!["tikv-server"])),
            WorkloadType::IoLatency
        );
        // A database still outranks the web tier on a combined host.
        assert_eq!(
            classify(&make_info(vec!["tidb-server", "nginx"])),
            WorkloadType::IoLatency
        );
    }

    #[test]
    fn test_classify_clickhouse() {
        assert_eq!(
            classify(&make_info(vec!["clickhouse"])),
            WorkloadType::IoLatency
        );
    }

    #[test]
    fn test_classify_elasticsearch() {
        assert_eq!(
            classify(&make_info(vec!["elasticsearch"])),
            WorkloadType::MemoryIntensive
        );
    }

    #[test]
    fn test_classify_coordination_kv() {
        // ZooKeeper and Consul are the same in-memory coordination-KV class
        // as etcd - which classify already routes to memory-intensive - and
        // both are services the engine detects (the runtime marker table
        // resolves org.apache.zookeeper to "zookeeper"). A ZooKeeper-only
        // host must not classify as mixed.
        assert_eq!(
            classify(&make_info(vec!["zookeeper"])),
            WorkloadType::MemoryIntensive
        );
        assert_eq!(
            classify(&make_info(vec!["consul"])),
            WorkloadType::MemoryIntensive
        );
        // A database still outranks coordination on a combined host.
        assert_eq!(
            classify(&make_info(vec!["zookeeper", "postgres"])),
            WorkloadType::IoLatency
        );
    }

    #[test]
    fn test_classify_etcd() {
        assert_eq!(
            classify(&make_info(vec!["etcd"])),
            WorkloadType::MemoryIntensive
        );
    }

    #[test]
    fn test_classify_caddy() {
        assert_eq!(
            classify(&make_info(vec!["caddy"])),
            WorkloadType::NetworkIntensive
        );
    }

    #[test]
    fn test_classify_compiler_ignored() {
        // Compilers are transient — should not affect classification
        assert_eq!(classify(&make_info(vec!["cc1"])), WorkloadType::Mixed);
        assert_eq!(classify(&make_info(vec!["rustc"])), WorkloadType::Mixed);
    }

    #[test]
    fn test_classify_streaming() {
        assert_eq!(
            classify(&make_info(vec!["kafka"])),
            WorkloadType::IoThroughput
        );
        assert_eq!(
            classify(&make_info(vec!["flink"])),
            WorkloadType::IoThroughput
        );
    }

    #[test]
    fn test_classify_pulsar() {
        // Pulsar is the same event-streaming platform class as Kafka (the
        // runtime detector even resolves org.apache.pulsar broker cmdlines
        // to the "pulsar" service), so a Pulsar-only host is an
        // io-throughput workload, not mixed.
        assert_eq!(
            classify(&make_info(vec!["pulsar"])),
            WorkloadType::IoThroughput
        );
        // A database on the same host still outranks the streaming tier.
        assert_eq!(
            classify(&make_info(vec!["pulsar", "postgres"])),
            WorkloadType::IoLatency
        );
    }

    #[test]
    fn test_classify_empty() {
        assert_eq!(classify(&make_info(vec![])), WorkloadType::Mixed);
    }

    #[test]
    fn test_classify_priority_db_over_web() {
        assert_eq!(
            classify(&make_info(vec!["nginx", "postgres"])),
            WorkloadType::IoLatency
        );
    }

    #[test]
    fn test_classify_priority_db_with_compiler() {
        // Compiler is transient, db takes priority
        assert_eq!(
            classify(&make_info(vec!["cc1", "postgres", "nginx"])),
            WorkloadType::IoLatency
        );
    }
}
