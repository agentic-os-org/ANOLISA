//! 采集器的增量状态投影，不读取对话正文或解析 ATIF。

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use super::{TrajectoryRecord, TrajectoryStore};

/// 增量状态独立于可裁剪的正文行；旧库只需增加表，不改写已有轨迹。
pub(super) fn initialize(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS source_session_states (
             session_id TEXT NOT NULL,
             source TEXT NOT NULL,
             file_path TEXT NOT NULL,
             file_size INTEGER NOT NULL,
             file_mtime_ns INTEGER NOT NULL,
             PRIMARY KEY (session_id, source, file_path)
         );
         CREATE INDEX IF NOT EXISTS idx_source_session_state_age
             ON source_session_states(file_mtime_ns);
         INSERT OR IGNORE INTO source_session_states
             (session_id, source, file_path, file_size, file_mtime_ns)
         SELECT session_id, source, file_path, file_size, file_mtime_ns
             FROM collected_trajectories WHERE source = 'hermes';",
    )?;
    Ok(())
}

/// 与正文写入共用事务，失败时不留下“已采集”的错误指纹。
pub(super) fn record(connection: &Connection, trajectory: &TrajectoryRecord) -> Result<()> {
    connection.execute(
        "INSERT INTO source_session_states
             (session_id, source, file_path, file_size, file_mtime_ns)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(session_id, source, file_path) DO UPDATE SET
             file_size = excluded.file_size, file_mtime_ns = excluded.file_mtime_ns",
        params![
            trajectory.session_id,
            trajectory.source,
            trajectory.file_path,
            trajectory.file_size,
            trajectory.file_mtime_ns
        ],
    )?;
    Ok(())
}

impl TrajectoryStore {
    /// 按会话及来源定位指纹，容量裁剪后的会话仍保留状态。
    pub(crate) fn get_source_session_state(
        &self,
        session_id: &str,
        source: &str,
        file_path: &str,
    ) -> Result<Option<(i64, i64)>> {
        self.lock_conn()?
            .query_row(
                "SELECT file_size, file_mtime_ns FROM source_session_states
                 WHERE session_id = ?1 AND source = ?2 AND file_path = ?3",
                params![session_id, source, file_path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    /// 清理源已过期或源库已消失的状态，不能因正文仍存在而提前删除状态。
    pub(crate) fn remove_stale_source_session_states(
        &self,
        cutoff_ns: Option<i64>,
    ) -> Result<usize> {
        let mut connection = self.lock_conn()?;
        let transaction = connection.transaction()?;
        let mut removed = if let Some(cutoff) = cutoff_ns {
            transaction.execute(
                "DELETE FROM source_session_states WHERE file_mtime_ns < ?1",
                params![cutoff],
            )?
        } else {
            0
        };
        let missing = {
            let mut statement =
                transaction.prepare("SELECT DISTINCT file_path FROM source_session_states")?;
            let paths = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            paths
                .into_iter()
                .filter(|path| !std::path::Path::new(path).exists())
                .collect::<Vec<_>>()
        };
        for path in missing {
            removed += transaction.execute(
                "DELETE FROM source_session_states WHERE file_path = ?1",
                params![path],
            )?;
        }
        transaction.commit()?;
        Ok(removed)
    }
}
