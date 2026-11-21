//! Optional same-user bridge from AW's public audit export into AgentSight's private store.

mod protocol;

use anyhow::{Context, Result, ensure};
use protocol::{Reply, check_anchor, is_identity};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

use crate::storage::{
    Storage,
    aw_audit::{AwAuditStore, Chain, Page},
};

pub use protocol::Export;

struct Consumer {
    export: Export,
    store: AwAuditStore,
    source: Option<String>,
    inventory: Option<Value>,
}

/// Linux serve owns this worker and joins it on shutdown.
pub struct Worker {
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    /// Start importing public audit pages using the effective user of AgentSight.
    pub fn start(export: Export, storage_base: &Path) -> Result<Self> {
        let mut consumer = Consumer::open(export, storage_base)?;
        let running = Arc::new(AtomicBool::new(true));
        let keep_running = Arc::clone(&running);
        let handle = thread::Builder::new()
            .name("aw-audit-import".into())
            .spawn(move || {
                let mut failed = false;
                while keep_running.load(Ordering::Acquire) {
                    match consumer.poll(&keep_running) {
                        Ok(()) => {
                            if failed {
                                log::info!("AW audit export resumed");
                            }
                            failed = false;
                        }
                        Err(error) => {
                            if !failed {
                                log::warn!("AW audit import interrupted: {error}");
                            }
                            failed = true;
                        }
                    }
                    thread::park_timeout(std::time::Duration::from_secs(if failed {
                        5
                    } else {
                        1
                    }));
                }
            })?;
        Ok(Self {
            running,
            handle: Some(handle),
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            handle.thread().unpark();
            if handle.join().is_err() {
                log::error!("AW audit import worker panicked");
            }
        }
    }
}

impl Consumer {
    fn open(export: Export, storage_base: &Path) -> Result<Self> {
        let store = Storage::open_aw_audit(storage_base)?;
        let (binary, state_dir) = export.selection();
        let source = store.configured_source(binary, state_dir)?;
        if let Some(source) = &source {
            store.status(source, "unknown")?;
        }
        Ok(Self {
            export,
            store,
            source,
            inventory: None,
        })
    }

    fn poll(&mut self, running: &AtomicBool) -> Result<()> {
        let result = self.sync(running);
        if result.is_err()
            && let Some(source) = &self.source
        {
            self.store.status(source, "unavailable")?;
        }
        result
    }

    fn sync(&mut self, running: &AtomicBool) -> Result<()> {
        let response = self
            .export
            .call(json!({"type":"enumerate","limit":64,"cursor":self.inventory}))?;
        if let Some(error) = response.error {
            if error == "source_changed" {
                self.inventory = None;
                if let Some(previous) = &self.source {
                    self.store.status(previous, "source_changed")?;
                }
                return Ok(());
            }
            anyhow::bail!("AW audit inventory unavailable ({})", reason(&error));
        }
        let source = response
            .source_id
            .context("AW audit inventory has no source")?;
        let Some(Reply::Enumerate { keys, next_cursor }) = response.result else {
            anyhow::bail!("expected AW audit inventory");
        };
        ensure!(
            keys.len() <= 64
                && keys.iter().all(|key| is_identity(key))
                && keys.windows(2).all(|pair| pair[0] < pair[1]),
            "invalid AW audit inventory"
        );
        if let Some(next) = &next_cursor {
            ensure!(
                next["source_id"] == source
                    && next["after"].as_str() == keys.last().map(String::as_str)
                    && keys.last().is_some(),
                "invalid AW inventory cursor"
            );
        }
        if self.source.as_ref() != Some(&source) {
            if let Some(previous) = &self.source {
                self.store.status(previous, "source_changed")?;
            }
            let (binary, state_dir) = self.export.selection();
            self.store.bind_source(binary, state_dir, &source)?;
            self.source = Some(source.clone());
        }
        self.store.discover(&source, &keys)?;
        self.inventory = next_cursor;
        for chain in self.store.next_chains(&source)? {
            if !running.load(Ordering::Acquire) {
                break;
            }
            self.read_chain(&source, &chain)?;
        }
        self.store.status(&source, "reachable")?;
        Ok(())
    }

    fn read_chain(&mut self, source: &str, chain: &Chain) -> Result<()> {
        let response = self.export.call(json!({
            "type":"read","limit":64,"chain_id":chain.id,"cursor":chain.cursor
        }));
        let response = match response {
            Ok(response) => response,
            Err(error) => return self.chain_failure(source, chain, "export_unavailable", error),
        };
        if let Some(error) = response.error {
            self.store.gap(source, chain, reason(&error))?;
            return Ok(());
        }
        ensure!(
            response.source_id.as_deref() == Some(source),
            "AW audit source changed"
        );
        let Some(Reply::Read {
            event,
            records,
            chain_id,
            cursor,
            tip,
            caught_up,
            terminal,
        }) = response.result
        else {
            anyhow::bail!("expected AW audit chain");
        };
        let page = Page {
            cursor: &cursor,
            done: caught_up && terminal,
            event: &event,
            records: &records,
        };
        let result = (|| {
            validate_page(source, chain, &chain_id, &tip, caught_up, &page)?;
            self.store.commit(source, chain, page)
        })();
        if let Err(error) = result {
            self.chain_failure(source, chain, "integrity_or_storage_error", error)?;
        }
        Ok(())
    }

    fn chain_failure(
        &self,
        source: &str,
        chain: &Chain,
        code: &str,
        error: anyhow::Error,
    ) -> Result<()> {
        self.store.gap(source, chain, code)?;
        if chain.gap.as_deref() != Some(code) {
            log::warn!("AW audit chain read interrupted: {error}");
        }
        Ok(())
    }
}

fn validate_page(
    source: &str,
    chain: &Chain,
    id: &str,
    tip: &Value,
    caught_up: bool,
    page: &Page<'_>,
) -> Result<()> {
    let previous = chain
        .cursor
        .as_ref()
        .map(|cursor| check_anchor(&cursor["consumed"]))
        .transpose()?;
    let consumed = check_anchor(&page.cursor["consumed"])?;
    let producer = &page.event["metadata"]["producer"];
    ensure!(
        id == chain.id
            && page.cursor["chain_id"] == id
            && page.cursor["source_id"] == source
            && page.cursor["observed_tip"] == *tip
            && consumed <= check_anchor(tip)?
            && caught_up == (page.cursor["consumed"] == *tip)
            && previous.is_none_or(|seq| consumed > seq
                || consumed == seq
                    && page.cursor["consumed"]
                        == *chain
                            .cursor
                            .as_ref()
                            .map_or(&Value::Null, |c| &c["consumed"]))
            && page.records.len() <= 64,
        "inconsistent AW audit cursor"
    );
    ensure!(
        page.event["kind"] == "ExecutionEvent"
            && page.event["apiVersion"] == "runtime.aw/v1alpha1"
            && page.event["metadata"]["uid"] == id
            && producer["scopeId"] == source
            && producer["producerId"] == "aw-service"
            && producer["generation"].is_string(),
        "invalid AW audit execution event"
    );
    let mut last = previous;
    for record in page.records {
        let data = &record["data"];
        let sequence = data["chain"]["sequence"]
            .as_u64()
            .context("invalid AW audit record sequence")?;
        ensure!(
            record["kind"] == "AuditRecord"
                && record["apiVersion"] == "audit.aw/v1alpha1"
                && record["metadata"]["uid"].as_str().is_some_and(is_identity)
                && record["metadata"]["producer"] == *producer
                && data["chain"]["id"] == id
                && data["eventRef"]["uid"] == id
                && data["eventRef"]["producer"] == *producer
                && data["subject"] == page.event["data"]["subject"]
                && sequence <= consumed
                && last.is_none_or(|previous| sequence > previous),
            "invalid AW audit record or association"
        );
        last = Some(sequence);
    }
    Ok(())
}

fn reason(code: &str) -> &str {
    if code.len() <= 64
        && !code.is_empty()
        && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
    {
        code
    } else {
        "invalid_export_error"
    }
}

/// Read a source's derived timeline and ingestion gaps using a bounded UTC time range.
pub fn query(
    base: &Path,
    source: &str,
    instance: Option<&str>,
    from_ms: i64,
    to_ms: i64,
) -> Result<Value> {
    ensure!(
        is_identity(source) && from_ms < to_ms,
        "invalid AW source or time range"
    );
    Storage::query_aw_audit(base)?.query(source, instance, from_ms, to_ms)
}

#[cfg(test)]
mod tests;
