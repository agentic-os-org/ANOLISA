//! Capacity accounting must not confuse a reusable WAL with live database pages.

use super::checkpoint::*;

#[test]
fn live_page_measurement_observes_uncheckpointed_growth_and_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("live.db");
    let conn = agentsight_sqlite_lifecycle::open_connection(
        &path,
        agentsight_sqlite_lifecycle::ConnectionOptions::default(),
    )
    .unwrap();
    agentsight_sqlite_lifecycle::disable_auto_checkpoint(&conn).unwrap();
    conn.execute_batch("CREATE TABLE payloads (data BLOB);")
        .unwrap();
    let empty = live_bytes(&conn).unwrap();
    conn.execute_batch("INSERT INTO payloads VALUES (zeroblob(2097152));")
        .unwrap();
    let full = live_bytes(&conn).unwrap();
    assert!(full > empty + 2_000_000);
    conn.execute_batch("DELETE FROM payloads;").unwrap();
    assert_eq!(live_bytes(&conn).unwrap(), empty);
    assert!(
        std::fs::metadata(path.with_extension("db-wal"))
            .unwrap()
            .len()
            > full
    );
}
