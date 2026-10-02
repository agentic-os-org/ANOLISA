//! Existing `SkillFS` v1 HMAC domains and frame bytes; signing keys are never used here.

use super::SkillFsError;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::{
    hmac,
    rand::{SecureRandom as _, SystemRandom},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub(super) const NOTIFY_CLIENT: &str = "anolisa.skillfs.notify.client.v1";
pub(super) const NOTIFY_SERVER: &str = "anolisa.skillfs.notify.server.v1";
pub(super) const CONTROL_CLIENT: &str = "anolisa.skillfs.control.client.v1";
pub(super) const CONTROL_SERVER: &str = "anolisa.skillfs.control.server.v1";

#[derive(Clone)]
pub(super) struct Secret(pub Arc<[u8]>);

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Frame {
    auth_version: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    nonce: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    proof: Option<String>,
}

impl Frame {
    pub(super) fn encode(kind: &str, nonce: Option<&[u8]>, proof: Option<&[u8]>) -> Vec<u8> {
        let mut value = serde_json::json!({"authVersion":"1","type":kind});
        if let Some(nonce) = nonce {
            value["nonce"] = STANDARD.encode(nonce).into();
        }
        if let Some(proof) = proof {
            value["proof"] = STANDARD.encode(proof).into();
        }
        value.to_string().into_bytes()
    }

    pub(super) fn parse(bytes: &[u8], kind: &str) -> Result<Self, SkillFsError> {
        if bytes.len() > 4096 {
            return Err(SkillFsError::Authentication);
        }
        let frame: Self =
            serde_json::from_slice(bytes).map_err(|_| SkillFsError::Authentication)?;
        if frame.auth_version != "1"
            || frame.kind != kind
            || frame.nonce.is_some() != (kind == "auth.challenge")
            || frame.proof.is_some() != matches!(kind, "auth.proof" | "auth.ok" | "auth.frame")
        {
            return Err(SkillFsError::Authentication);
        }
        Ok(frame)
    }

    pub(super) fn nonce(&self) -> Result<[u8; 32], SkillFsError> {
        decode(self.nonce.as_deref())
    }
    pub(super) fn proof(&self) -> Result<[u8; 32], SkillFsError> {
        decode(self.proof.as_deref())
    }
}

fn decode(value: Option<&str>) -> Result<[u8; 32], SkillFsError> {
    let value = value.ok_or(SkillFsError::Authentication)?;
    let bytes = STANDARD
        .decode(value)
        .map_err(|_| SkillFsError::Authentication)?;
    if STANDARD.encode(&bytes) != value {
        return Err(SkillFsError::Authentication);
    }
    bytes.try_into().map_err(|_| SkillFsError::Authentication)
}

pub(super) fn nonce() -> Result<[u8; 32], SkillFsError> {
    let mut nonce = [0; 32];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| SkillFsError::Authentication)?;
    Ok(nonce)
}

fn input(domain: &str, nonce: &[u8; 32], payload: Option<&[u8]>) -> Vec<u8> {
    let mut input = domain.as_bytes().to_vec();
    if let Some(payload) = payload {
        input.extend_from_slice(b"\0frame\0");
        input.extend_from_slice(nonce);
        input.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        input.extend_from_slice(payload);
    } else {
        input.push(0);
        input.extend_from_slice(nonce);
    }
    input
}

pub(super) fn sign(
    secret: &Secret,
    domain: &str,
    nonce: &[u8; 32],
    payload: Option<&[u8]>,
) -> hmac::Tag {
    hmac::sign(
        &hmac::Key::new(hmac::HMAC_SHA256, &secret.0),
        &input(domain, nonce, payload),
    )
}

pub(super) fn verify(
    secret: &Secret,
    domain: &str,
    nonce: &[u8; 32],
    payload: Option<&[u8]>,
    proof: &[u8],
) -> Result<(), SkillFsError> {
    hmac::verify(
        &hmac::Key::new(hmac::HMAC_SHA256, &secret.0),
        &input(domain, nonce, payload),
        proof,
    )
    .map_err(|_| SkillFsError::Authentication)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_skillfs_frozen_domains_and_rejects_tamper_replay_and_noncanonical_tags() {
        let key = Secret((0_u8..32).collect::<Vec<_>>().into());
        let nonce: [u8; 32] = (32_u8..64).collect::<Vec<_>>().try_into().unwrap();
        let payload = br#"{"schemaVersion":"1","method":"ping"}"#;
        for (domain, handshake, frame) in [
            (
                CONTROL_CLIENT,
                "pqaSiunq07XWqMvQ8xSiSLi6dsLEy5iaCEF3md04AVI=",
                "zT6arzIjdC4fJqiSM59qNhU2BADHJgFRq8YqifcdHCM=",
            ),
            (
                CONTROL_SERVER,
                "naSgjgOT+Zs71EytW6byhJMCkfek2sGmK+CDqHmDsas=",
                "W9lF84f43ROoMXPHkgovtowDiZ5zv0zubs2vmq98T9k=",
            ),
            (
                NOTIFY_CLIENT,
                "aFcVadTie7FrVTYOjk1OOjBpoQZ6LUvnLGC6stiqt6M=",
                "Zzf/dpWsuj89DFbpJtwqBk5dHsV+GXwGOytZMh5xDKw=",
            ),
            (
                NOTIFY_SERVER,
                "F22J+ua0Pmha2dPyTMmTQNtKjcmed59Mo8FKgdcgBOc=",
                "ut2Pv/8XHmiImbWSfm53ixIcoDimZOPHzrS6g3TtO+M=",
            ),
        ] {
            assert_eq!(STANDARD.encode(sign(&key, domain, &nonce, None)), handshake);
            let tag = sign(&key, domain, &nonce, Some(payload));
            assert_eq!(STANDARD.encode(tag), frame);
            verify(&key, domain, &nonce, Some(payload), tag.as_ref()).unwrap();
            assert!(verify(&key, domain, &nonce, Some(b"tampered"), tag.as_ref()).is_err());
            assert!(verify(&key, domain, &[0; 32], Some(payload), tag.as_ref()).is_err());
        }
        assert!(
            Frame::parse(
                br#"{"authVersion":"1","type":"auth.init","uid":0}"#,
                "auth.init"
            )
            .is_err()
        );
        assert!(decode(Some("aFcVadTie7FrVTYOjk1OOjBpoQZ6LUvnLGC6stiqt6M")).is_err());
    }
}
