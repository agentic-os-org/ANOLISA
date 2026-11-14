//! Exact published OpenClaw modules required by the final result checkpoints.

use crate::read_file;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{ffi::OsStr, path::Path};

struct BundleProfile {
    entry_digest: &'static str,
    core: (&'static str, &'static str),
    transport_digest: &'static str,
    dependencies: [(&'static str, &'static str); 7],
}

fn bundle_profile(version: &str) -> Option<BundleProfile> {
    match version {
        "2026.9.5" => Some(BundleProfile {
            entry_digest: "538e8ee2b65a0b24bb8a5ed3421bfe66621b1e0b5f726a167758c004f566fb36",
            core: (
                "agent-core-iKDcH8nh.mjs",
                "990da1e3b53940558a6996f8631a8774bb07639ec92d8a614bdf30aab7a505cf",
            ),
            transport_digest: "c536d07188bbe9479f31d1a4733bd90f1f9404317afd6e38725a79c9296e445f",
            dependencies: [
                (
                    "dist/attempt.model-diagnostic-events-cm5plRPy.mjs",
                    "4bb03fdf79a2d16f094ce168577c79cd7dfe6483e89d6adf0e490db34917d7eb",
                ),
                (
                    "dist/builtin-openclaw-C7-lQJ2Z.mjs",
                    "97435362096a8903b3a01ee82869b55b4375c371db4be3bcc32b641a9074bcca",
                ),
                (
                    "dist/resource-loader-JTzqS2Ec.mjs",
                    "7efd5e9d6b694092eaf9206ffdcaf43e08dae07b2a0393142affc3e285d32959",
                ),
                (
                    "dist/session-DV4NUaHW.mjs",
                    "e1ba1e803790bbb7903b1908c17b99bc7626f0e24b6fba307f9d812d39df59dd",
                ),
                (
                    "dist/tool-call-id-DaXTcHcK.mjs",
                    "8e443694a2317d2817fe042dc16b3a1c43903d9232485d33724efa106ebedde8",
                ),
                (
                    "@openclaw/ai/dist/openai-completions-stream-Bu98b0Hv.mjs",
                    "761ac664eaf802b14fa0d6f482253f4e8846bc684526ff55ace660fdc568c7ce",
                ),
                (
                    "@openclaw/ai/dist/host-B4MeUNBc.mjs",
                    "2e1bbb4a54a7386a69d3a97787f349968f2e881fc085a4643861fbfbf2387b87",
                ),
            ],
        }),
        "2026.9.6" => Some(BundleProfile {
            entry_digest: "4dd36d2ed0cb154456c4a09a951a2e8f2508a5dd6ff2fe0688d4e1c8f773fcb5",
            core: (
                "agent-core-DG5aPUnb.mjs",
                "96221c65ea8e15d6499ca9f817e5d7629151b42c6d9742000791f2f48f2cebcd",
            ),
            transport_digest: "038e2c72e79a3e1c9d06e9c5f3a61a13acc297e1026696da24fee634a3bc4abc",
            dependencies: [
                (
                    "dist/attempt.model-diagnostic-events-B1vBuHmP.mjs",
                    "4f2cb3e7bcb05424da43b2f8f073d7afb6a1248fe09bff6b7e7d226c3712bbd7",
                ),
                (
                    "dist/builtin-openclaw-q1hiFm14.mjs",
                    "e4504516aef56cd293e11369e420148c5ad60c80e60128ed64bfb57fb7239e25",
                ),
                (
                    "dist/resource-loader-B1DeXClE.mjs",
                    "5a44e24ac53be932e5b4875171fecf739b06bf0992ed6ffd0314899d4b05e867",
                ),
                (
                    "dist/session-D9cHHQGH.mjs",
                    "2f571cf4d56b6c80cf60b6a92fbd9a941955f0cf94d3e77658a5e30790e5fa8e",
                ),
                (
                    "dist/tool-call-id-Dp16BALg.mjs",
                    "1620a9953ee30bcb02febc1b9d968997b4ffbaccef9cce56596c686e89d1ddd2",
                ),
                (
                    "@openclaw/ai/dist/openai-completions-stream-BpuJ0ZOA.mjs",
                    "fa04ec26835dcc7afdf1ecb8e89a5d7299222b55cc15a778fcfacfd417123287",
                ),
                (
                    "@openclaw/ai/dist/host-yTvBrYM2.mjs",
                    "5ac29e306027cd9c0c9a5be052a69a2613dd18354ee8d5de66cf39e37146bd17",
                ),
            ],
        }),
        "2026.9.7" => Some(BundleProfile {
            entry_digest: "e4a0f34ab2c0b01be203f8e385db286beec19649f6bdc751065de445ca2c38dd",
            core: (
                "agent-core-yAMLnNkR.mjs",
                "28bd7caff972c4fc5dbcfa14d88e45be4795ef79809342295056aa3ca54bf72a",
            ),
            transport_digest: "31ac730235a155a5a662096c3d467554e07b66a456b05b1971a081379ab4bc51",
            dependencies: [
                (
                    "dist/session-boundary-prompt-cache-key-CDZeB8W0.mjs",
                    "08cdd0961479d778f0d7385f7080bed5f25623e1f1f47b0db4fc6dff61202a13",
                ),
                (
                    "dist/builtin-openclaw-dCw2mRD9.mjs",
                    "7c408f516754ffb156c23d27727a325fa56785c8d514276d79bd843053712a38",
                ),
                (
                    "dist/resource-loader-D_dZVBFw.mjs",
                    "a187b7ade063f60d9805920fea25e7073ee6d92637b2f81425a7de6ff326792e",
                ),
                (
                    "dist/session-BA9qunLx.mjs",
                    "76e83df0e3056d87ac01eeb929a1b1f5a4722ef274653e3bafef4c64fc9468cd",
                ),
                (
                    "dist/tool-call-id-iLO2-UNe.mjs",
                    "42486db2c5848aa90a3e666136ca7fbf391ea5ea31198dbe9c10a7496c1893cf",
                ),
                (
                    "@openclaw/ai/dist/openai-completions-stream-CDCuw3DP.mjs",
                    "02b71138b68ed200dfc4508d3a17855edc979d9b89bff3dcb3e69bcfc2827f90",
                ),
                (
                    "@openclaw/ai/dist/host-CHi3X87F.mjs",
                    "66692e8958fee7e114074bd459f3c5e09257bf786452ec90c8a2551ad3b564ee",
                ),
            ],
        }),
        _ => None,
    }
}

pub(super) fn verified_bundle(command: &aw_exec::CommandSpec, version: &str) -> bool {
    let Some(profile) = bundle_profile(version) else {
        return false;
    };
    let Some(entry) = command.args.first() else {
        return false;
    };
    if Path::new(entry).file_name() != Some(OsStr::new("openclaw.mjs")) {
        return false;
    }
    let Some(root) = Path::new(entry)
        .parent()
        .and_then(|path| path.canonicalize().ok())
    else {
        return false;
    };
    if !root.ends_with("node_modules/openclaw") {
        return false;
    }
    let expected_entry = root.join("openclaw.mjs");
    if Path::new(entry).canonicalize().ok().as_deref() != Some(expected_entry.as_path()) {
        return false;
    }
    let Ok(entry_source) = read_file(Path::new(entry), 1024 * 1024) else {
        return false;
    };
    if format!("{:x}", Sha256::digest(entry_source)) != profile.entry_digest {
        return false;
    }
    let Ok(metadata) = read_file(root.join("package.json"), 1024 * 1024)
        .and_then(|bytes| Ok(serde_json::from_slice::<Value>(&bytes)?))
    else {
        return false;
    };
    let bundle = root.join("dist").join(profile.core.0);
    if bundle.canonicalize().ok().as_deref() != Some(bundle.as_path()) {
        return false;
    }
    let Ok(source) = read_file(&bundle, 1024 * 1024) else {
        return false;
    };
    if metadata["version"] != version || format!("{:x}", Sha256::digest(source)) != profile.core.1 {
        return false;
    }
    let Some(ai_root) = root.parent().map(|parent| parent.join("@openclaw/ai")) else {
        return false;
    };
    let Ok(ai_package) = read_file(ai_root.join("package.json"), 1024 * 1024)
        .and_then(|bytes| Ok(serde_json::from_slice::<Value>(&bytes)?))
    else {
        return false;
    };
    if ai_package["version"] != version {
        return false;
    }
    let dependencies = profile.dependencies.into_iter().map(|(path, digest)| {
        if let Some(path) = path.strip_prefix("@openclaw/ai/") {
            (ai_root.join(path), digest)
        } else {
            (root.join(path), digest)
        }
    });
    for (path, digest) in dependencies.chain(std::iter::once((
        ai_root.join("dist/transports.mjs"),
        profile.transport_digest,
    ))) {
        if path.canonicalize().ok().as_deref() != Some(path.as_path())
            || read_file(&path, 1024 * 1024)
                .map(|bytes| format!("{:x}", Sha256::digest(bytes)) != digest)
                .unwrap_or(true)
        {
            return false;
        }
    }
    true
}
