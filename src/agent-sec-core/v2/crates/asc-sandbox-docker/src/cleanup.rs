//! Register ownership before create; release mounts and config only after confirmed deletion.
use crate::{ContainerPlan, DockerError, client::Client};
use std::time::Duration;

pub(crate) struct Cleanup<'a, T> {
    client: &'a Client,
    name: &'a str,
    staging: Option<T>,
    pub(crate) creation_confirmed: bool,
    armed: bool,
}

impl<'a, T> Cleanup<'a, T> {
    pub(crate) fn new(
        client: &'a Client,
        name: &'a str,
        plan: &ContainerPlan,
        staging: T,
    ) -> Result<Self, DockerError> {
        let record = serde_json::json!({
            "container": name, "endpoint": client.host,
            "config": client.directory,
            "staging": plan.mounts.iter().map(|mount| &mount.source).collect::<Vec<_>>(),
            "state": "unknown-until-cleanup-confirmed",
                        "recovery_argv": ["docker", "--host", client.host.as_str(), "--config", &client.directory.to_string_lossy(), "rm", "-f", name]
        });
        std::fs::write(
            client.directory.join("recovery.json"),
            serde_json::to_vec(&record)?,
        )?;
        Ok(Self {
            client,
            name,
            staging: Some(staging),
            creation_confirmed: false,
            armed: true,
        })
    }

    pub(crate) fn finish<R>(&mut self, outcome: Result<R, DockerError>) -> Result<R, DockerError> {
        self.release()?;
        outcome
    }

    fn release(&mut self) -> Result<(), DockerError> {
        self.armed = false;
        let removed = remove(self.client, self.name);
        if removed.is_err() || !self.creation_confirmed {
            self.client.preserve();
            if let Some(staging) = self.staging.take() {
                std::mem::forget(staging);
            }
            let error = DockerError::Protocol(format!(
                "cleanup unconfirmed; container={} endpoint={} recovery={}; use recovery_argv in the record; do not release staging until creation has settled and absence is confirmed",
                self.name,
                self.client.host,
                self.client.directory.join("recovery.json").display()
            ));
            eprintln!("{error}");
            return Err(error);
        }
        self.staging.take();
        Ok(())
    }
}

impl<T> Drop for Cleanup<'_, T> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.release();
        }
    }
}

fn remove(client: &Client, name: &str) -> Result<(), DockerError> {
    for _ in 0..3 {
        let _ = client.output(client.command(&["rm", "-f", name]), Duration::from_secs(10));
        let filter = format!("name=^/{name}$");
        let output = client.output(
            client.command(&["ps", "--all", "--filter", &filter, "--format", "{{.ID}}"]),
            Duration::from_secs(10),
        );
        if output.is_ok_and(|reply| {
            reply.status.success() && reply.stdout.iter().all(u8::is_ascii_whitespace)
        }) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(DockerError::Protocol(
        "container absence could not be confirmed".into(),
    ))
}
