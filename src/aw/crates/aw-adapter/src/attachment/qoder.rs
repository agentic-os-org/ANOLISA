//! Confirm removal through a newly adopted ConfigChange route without sending a prompt.

use super::{files, merge, Registration};
use crate::Result;
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    time::{Duration, Instant},
};

impl Registration {
    pub(super) fn detach_qoder(&self) -> Result<()> {
        let Self::Qoder {
            settings,
            groups,
            confirmation,
            ..
        } = self
        else {
            return Err("not a Qoder registration".into());
        };
        let directory = confirmation
            .parent()
            .ok_or("missing attachment directory")?;
        let description = directory.join("detach-confirmation.json");
        let receipt = directory.join("detach-ready.json");
        let token = fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .to_owned();
        if receipt.try_exists()? {
            fs::remove_file(&receipt)?;
        }
        let value = json!({"settings":settings,"ready":receipt,"token":token,"detaching":true});
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&description)?;
        file.write_all(&serde_json::to_vec(&value)?)?;
        file.sync_all()?;
        let mut marker = groups["hooks"]["ConfigChange"][0].clone();
        let args = marker["hooks"][0]["args"]
            .as_array_mut()
            .ok_or("invalid confirmation command")?;
        *args.last_mut().ok_or("missing confirmation path")? = json!(description);
        // Both phases belong to this attachment, including probes left by a
        // timed-out detach. Validate and remove them in one settings update.
        let mut owned = groups.clone();
        owned["hooks"]["ConfigChange"]
            .as_array_mut()
            .ok_or("invalid confirmation groups")?
            .push(marker.clone());
        let removal = Self::Qoder {
            settings: settings.clone(),
            created: false,
            groups: json!({"hooks":{"ConfigChange":[marker]}}),
            confirmation: description,
            token: token.clone(),
            ready: receipt.clone(),
        };
        Self::remove_qoder_groups(settings, &owned)?;
        if let Self::Qoder { groups, .. } = &removal {
            files::update(settings, |value| merge(value, groups))?;
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        for sequence in 0..15 {
            std::thread::sleep(Duration::from_millis(500));
            removal.probe(sequence)?;
            if receipt.try_exists()? {
                let ack: Value = serde_json::from_slice(&files::read(&receipt)?)?;
                if ack["token"] != token || ack["adapter"] != "qoder" {
                    return Err("detach confirmation mismatch".into());
                }
                removal.remove_groups()?;
                return Ok(());
            }
            if Instant::now() >= deadline {
                break;
            }
        }
        Err("Qoder detach confirmation timed out; retry removal before dispatching requests".into())
    }
}
