//! QwenPaw's native API loads the existing AW middleware and startup receipt.

use super::{files, Registration};
use crate::Result;
use serde_json::{json, Value};
use std::{io::Read, time::Duration};

impl Registration {
    fn response(
        &self,
        method: reqwest::Method,
        route: &str,
        body: Option<Value>,
    ) -> Result<reqwest::blocking::Response> {
        let Self::Qwenpaw { port, .. } = self else {
            return Err("not a QwenPaw registration".into());
        };
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|error| error.to_string())?;
        let mut request = client.request(
            method,
            format!("http://127.0.0.1:{port}/api/plugins{route}"),
        );
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/json")
                .body(serde_json::to_vec(&body)?);
        }
        request
            .send()
            .map_err(|error| format!("QwenPaw native plugin API: {error}").into())
    }

    fn api(&self, method: reqwest::Method, route: &str, body: Option<Value>) -> Result<Value> {
        let mut response = self
            .response(method, route, body)?
            .error_for_status()
            .map_err(|error| format!("QwenPaw native plugin API: {error}"))?;
        let mut bytes = Vec::new();
        response
            .by_ref()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 1024 * 1024 {
            return Err("QwenPaw API response exceeds limit".into());
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub(super) fn install_plugin(&self) -> Result<()> {
        let Self::Qwenpaw {
            profile, source, ..
        } = self
        else {
            return Err("not a QwenPaw registration".into());
        };
        if profile.join("plugins/aw-native").try_exists()? {
            return Err("QwenPaw AW plugin already exists".into());
        }
        let result = self.api(
            reqwest::Method::POST,
            "/install",
            Some(json!({"source":source,"force":false})),
        )?;
        if result["id"] != "aw-native" || result["loaded"] != true {
            return Err("QwenPaw did not report loaded AW plugin".into());
        }
        self.check_plugin()
    }

    fn check_plugin(&self) -> Result<()> {
        let Self::Qwenpaw {
            profile,
            descriptor,
            ..
        } = self
        else {
            return Err("not a QwenPaw registration".into());
        };
        let path = profile.join("plugins/aw-native");
        files::directory(&path)?;
        let installed: Value =
            serde_json::from_slice(&files::read(&path.join("aw-attachment.json"))?)?;
        if &installed != descriptor
            || files::read(&path.join("plugin.py"))?
                != include_bytes!("../../native/qwenpaw/plugin.py")
            || files::read(&path.join("plugin.json"))?
                != include_bytes!("../../native/qwenpaw/plugin.json")
        {
            return Err(
                "native AW plugin was replaced; refusing to remove another installation".into(),
            );
        }
        Ok(())
    }

    pub(super) fn remove_plugin(&self) -> Result<()> {
        let Self::Qwenpaw { profile, .. } = self else {
            return Err("not a QwenPaw registration".into());
        };
        if !profile.join("plugins/aw-native").try_exists()? {
            // The pinned status API checks the live loader before disk. A 404
            // proves absence; disk absence alone never proves native unload.
            let status = self.response(reqwest::Method::GET, "/aw-native/status", None)?;
            if status.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(());
            }
            status
                .error_for_status()
                .map_err(|error| format!("QwenPaw native plugin status: {error}"))?;
            return Err("native AW plugin tree is missing; unload cannot be verified; restore the owned tree before retrying detach".into());
        }
        self.check_plugin()?;
        self.api(reqwest::Method::DELETE, "/aw-native", None)?;
        if profile.join("plugins/aw-native").try_exists()? {
            return Err("QwenPaw did not remove AW plugin".into());
        }
        Ok(())
    }
}
