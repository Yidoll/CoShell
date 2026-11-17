use coshell_backup::snapshot_manifest::{
    build_from_legacy, open_packages, parse_manifest, verify_complete, SnapshotMetadata,
};
use coshell_backup::snapshot_manifest::{
    build_manifest, SnapshotProtection, COMPLETE_MARKER, CONNECTIONS_PACKAGE, CREDENTIALS_PACKAGE,
    MANIFEST_FILE,
};

#[test]
fn manifest_describes_independent_snapshot_packages_without_secrets() {
    let connections = br#"{"version":1,"connections":[]}"#;
    let credentials = b"credential-package";
    let built = build_manifest(
        "snapshot-1",
        "2026-11-16T03:18:18Z",
        "0.1.0",
        SnapshotProtection::Encrypted,
        connections,
        Some(credentials),
        0,
        0,
    )
    .unwrap();
    assert_eq!(MANIFEST_FILE, "manifest.json");
    assert_eq!(CONNECTIONS_PACKAGE, "connections.pkg");
    assert_eq!(CREDENTIALS_PACKAGE, "credentials.pkg");
    assert_eq!(COMPLETE_MARKER, "complete.json");
    assert!(built.manifest_text().contains("snapshot-1"));
    assert!(built.manifest_text().contains("connections.pkg"));
    assert!(built.manifest_text().contains("credentials.pkg"));
    assert!(!built.manifest_text().contains("credential-package"));
    assert_eq!(built.manifest_sha256.len(), 64);
}

#[test]
fn plaintext_and_encrypted_modes_are_explicit() {
    let plain = build_manifest(
        "s",
        "2026-11-16T03:18:18Z",
        "0.1.0",
        SnapshotProtection::Plaintext,
        b"{}",
        None,
        0,
        0,
    )
    .unwrap();
    let encrypted = build_manifest(
        "s",
        "2026-11-16T03:18:18Z",
        "0.1.0",
        SnapshotProtection::Encrypted,
        b"{}",
        None,
        0,
        0,
    )
    .unwrap();
    assert!(plain.manifest_text().contains("\"protection\":\"none\""));
    assert!(encrypted
        .manifest_text()
        .contains("\"protection\":\"encrypted\""));
}

fn legacy(secret: bool, password: &[u8]) -> Vec<u8> {
    let value = if secret {
        br#"{"version":1,"connections":[],"groups":[],"host_keys":[],"secrets":[{"connection_id":"c","secret_type":"password","value":[115,101,99,114,101,116]}]}"#.as_slice()
    } else {
        br#"{"version":1,"connections":[],"groups":[],"host_keys":[],"secrets":[]}"#.as_slice()
    };
    coshell_backup::encode_optional(value, password).unwrap()
}

#[test]
fn split_packages_roundtrip_both_modes_and_optional_credentials() {
    for password in [b"".as_slice(), b"backup password".as_slice()] {
        for secret in [false, true] {
            let encoded = legacy(secret, password);
            let artifact = build_from_legacy(
                &encoded,
                password,
                SnapshotMetadata {
                    snapshot_id: "snapshot",
                    created_at: "2026-11-16T03:18:18Z",
                    app_version: "0.1.0",
                },
            )
            .unwrap();
            let manifest = parse_manifest(&artifact.manifest).unwrap();
            assert_eq!(manifest.includes_credentials, secret);
            assert_eq!(artifact.credentials_package.is_some(), secret);
            verify_complete(&artifact.manifest, &artifact.complete).unwrap();
            let opened = open_packages(
                &artifact.manifest,
                &artifact.complete,
                &artifact.connections_package,
                artifact.credentials_package.as_ref().map(|v| v.as_slice()),
                password,
            )
            .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&opened.payload).unwrap();
            assert_eq!(
                value["secrets"].as_array().unwrap().len(),
                usize::from(secret)
            );
            if !password.is_empty() {
                assert!(!String::from_utf8_lossy(&artifact.connections_package)
                    .contains("\"host_keys\""));
                assert!(open_packages(
                    &artifact.manifest,
                    &artifact.complete,
                    &artifact.connections_package,
                    artifact.credentials_package.as_ref().map(|v| v.as_slice()),
                    b"wrong"
                )
                .is_err());
            }
        }
    }
}

#[test]
fn untrusted_manifest_and_mixed_packages_are_rejected() {
    let build = |id: &str| {
        build_from_legacy(
            &legacy(true, b"password"),
            b"password",
            SnapshotMetadata {
                snapshot_id: id,
                created_at: "2026-11-16T03:18:18Z",
                app_version: "0.1.0",
            },
        )
        .unwrap()
    };
    let first = build("first");
    let second = build("second");
    assert!(open_packages(
        &first.manifest,
        &first.complete,
        &first.connections_package,
        None,
        b"password"
    )
    .is_err());
    let mut m: serde_json::Value = serde_json::from_slice(&first.manifest).unwrap();
    for path in [
        "../secret",
        "/secret",
        "https://example.org/secret",
        "connections.pkg/../a",
    ] {
        m["files"][0]["path"] = path.into();
        assert!(parse_manifest(&serde_json::to_vec(&m).unwrap()).is_err());
    }
    m = serde_json::from_slice(&first.manifest).unwrap();
    m["files"][1]["path"] = CONNECTIONS_PACKAGE.into();
    assert!(parse_manifest(&serde_json::to_vec(&m).unwrap()).is_err());
    // Even rewriting public file hashes and the completion digest cannot mix
    // encrypted packages from a different snapshot: their identity is in AEAD.
    m = serde_json::from_slice(&first.manifest).unwrap();
    m["files"][1]["sha256"] =
        coshell_backup::snapshot_manifest::sha256(second.credentials_package.as_ref().unwrap())
            .into();
    m["files"][1]["length"] = (second.credentials_package.as_ref().unwrap().len() as u64).into();
    let manifest = serde_json::to_vec(&m).unwrap();
    let mut complete: serde_json::Value = serde_json::from_slice(&first.complete).unwrap();
    complete["manifestSha256"] = coshell_backup::snapshot_manifest::sha256(&manifest).into();
    assert!(open_packages(
        &manifest,
        &serde_json::to_vec(&complete).unwrap(),
        &first.connections_package,
        second.credentials_package.as_ref().map(|v| v.as_slice()),
        b"password"
    )
    .is_err());
}

#[test]
fn memory_framing_rejects_extra_truncated_or_tampered_bytes() {
    let artifact = build_from_legacy(
        &legacy(true, b""),
        b"",
        SnapshotMetadata {
            snapshot_id: "first",
            created_at: "2026-11-16T03:18:18Z",
            app_version: "0.1.0",
        },
    )
    .unwrap();
    let packed = artifact.pack().unwrap();
    let restored = coshell_backup::snapshot_manifest::SnapshotArtifact::unpack(&packed).unwrap();
    assert_eq!(restored.manifest, artifact.manifest);
    assert_eq!(
        restored.credentials_package.as_deref(),
        artifact.credentials_package.as_deref()
    );
    let mut extra = packed.to_vec();
    extra.push(0);
    assert!(coshell_backup::snapshot_manifest::SnapshotArtifact::unpack(&extra).is_err());
    assert!(coshell_backup::snapshot_manifest::SnapshotArtifact::unpack(
        &packed[..packed.len() - 1]
    )
    .is_err());
    let mut tampered = packed.to_vec();
    *tampered.last_mut().unwrap() ^= 1;
    assert!(coshell_backup::snapshot_manifest::SnapshotArtifact::unpack(&tampered).is_err());
}

#[test]
fn malformed_metadata_and_completion_are_rejected() {
    for (id, time) in [
        ("../s", "2026-11-16T03:18:18Z"),
        ("s", "2026-02-29T03:18:18Z"),
        ("s", "2026-11-16T03:18:18+08:00"),
        ("s", "2026-11-16T24:18:18Z"),
    ] {
        assert!(build_manifest(
            id,
            time,
            "0.1.0",
            SnapshotProtection::Plaintext,
            b"{}",
            None,
            0,
            0
        )
        .is_err());
    }
    let artifact = build_from_legacy(
        &legacy(false, b""),
        b"",
        SnapshotMetadata {
            snapshot_id: "first",
            created_at: "2026-11-16T03:18:18Z",
            app_version: "0.1.0",
        },
    )
    .unwrap();
    assert!(verify_complete(&artifact.manifest, b"{}").is_err());
    let mut marker: serde_json::Value = serde_json::from_slice(&artifact.complete).unwrap();
    marker["snapshotId"] = "other".into();
    assert!(verify_complete(&artifact.manifest, &serde_json::to_vec(&marker).unwrap()).is_err());
    let mut manifest: serde_json::Value = serde_json::from_slice(&artifact.manifest).unwrap();
    manifest["formatVersion"] = 99.into();
    assert_eq!(
        parse_manifest(&serde_json::to_vec(&manifest).unwrap()).unwrap_err(),
        coshell_backup::BackupError::UnsupportedVersion
    );
}

#[test]
fn pack_rejects_public_artifact_mutation_before_persistence() {
    let build = || {
        build_from_legacy(
            &legacy(true, b""),
            b"",
            SnapshotMetadata {
                snapshot_id: "pack",
                created_at: "2026-11-16T03:18:18Z",
                app_version: "0.1.0",
            },
        )
        .unwrap()
    };
    let mut missing = build();
    missing.credentials_package = None;
    assert!(missing.pack().is_err());
    let mut changed = build();
    changed.connections_package.clear();
    assert!(changed.pack().is_err());
    let mut changed = build();
    changed.credentials_package.as_mut().unwrap()[0] ^= 1;
    assert!(changed.pack().is_err());
}

#[test]
fn encrypted_packages_reject_role_exchange_and_public_metadata_rewriting() {
    let artifact = build_from_legacy(
        &legacy(true, b"password"),
        b"password",
        SnapshotMetadata {
            snapshot_id: "bound",
            created_at: "2026-11-16T03:18:18Z",
            app_version: "0.1.0",
        },
    )
    .unwrap();
    for mode in [
        "role",
        "appVersion",
        "createdAt",
        "connectionCount",
        "includesCredentials",
    ] {
        let mut manifest: serde_json::Value = serde_json::from_slice(&artifact.manifest).unwrap();
        let (connections, credentials) = if mode == "role" {
            (
                artifact.credentials_package.as_ref().unwrap().as_slice(),
                Some(artifact.connections_package.as_slice()),
            )
        } else {
            (
                artifact.connections_package.as_slice(),
                Some(artifact.credentials_package.as_ref().unwrap().as_slice()),
            )
        };
        match mode {
            "appVersion" => manifest["appVersion"] = "99.0.0".into(),
            "createdAt" => manifest["createdAt"] = "2026-11-17T03:18:18Z".into(),
            "connectionCount" => manifest["connectionCount"] = 9.into(),
            "includesCredentials" => {
                manifest["includesCredentials"] = false.into();
                manifest["files"].as_array_mut().unwrap().pop();
            }
            _ => {}
        }
        manifest["files"][0]["sha256"] =
            coshell_backup::snapshot_manifest::sha256(connections).into();
        manifest["files"][0]["length"] = (connections.len() as u64).into();
        if mode != "includesCredentials" {
            manifest["files"][1]["sha256"] =
                coshell_backup::snapshot_manifest::sha256(credentials.unwrap()).into();
            manifest["files"][1]["length"] = (credentials.unwrap().len() as u64).into();
        }
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let mut complete: serde_json::Value = serde_json::from_slice(&artifact.complete).unwrap();
        complete["manifestSha256"] = coshell_backup::snapshot_manifest::sha256(&bytes).into();
        assert!(
            open_packages(
                &bytes,
                &serde_json::to_vec(&complete).unwrap(),
                connections,
                if mode == "includesCredentials" {
                    None
                } else {
                    credentials
                },
                b"password"
            )
            .is_err(),
            "{mode}"
        );
    }
}
