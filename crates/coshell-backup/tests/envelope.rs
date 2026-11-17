use coshell_backup::{decrypt, encrypt, BackupError, Envelope, MAX_PAYLOAD_BYTES};

#[test]
fn optional_password_roundtrips_explicit_plaintext_and_encrypted_formats() {
    let plain = coshell_backup::encode_optional(b"sensitive fixture", b"").unwrap();
    assert!(String::from_utf8_lossy(&plain).contains("sensitive fixture"));
    assert_eq!(
        coshell_backup::decode_optional(&plain, b"")
            .unwrap()
            .as_slice(),
        b"sensitive fixture"
    );
    assert!(coshell_backup::decode_optional(&plain, b"wrong-mode").is_err());
    let encrypted = coshell_backup::encode_optional(b"sensitive fixture", b"pass").unwrap();
    assert!(!String::from_utf8_lossy(&encrypted).contains("sensitive fixture"));
    assert_eq!(
        coshell_backup::decode_optional(&encrypted, b"pass")
            .unwrap()
            .as_slice(),
        b"sensitive fixture"
    );
    assert!(coshell_backup::decode_optional(&encrypted, b"").is_err());
    let mut changed: serde_json::Value = serde_json::from_slice(&plain).unwrap();
    changed["payload"] = serde_json::json!("tampered");
    assert_eq!(
        coshell_backup::decode_optional(&serde_json::to_vec(&changed).unwrap(), b"").unwrap_err(),
        BackupError::Authentication
    );
}

#[test]
fn roundtrips_and_is_authenticated() {
    let encoded = encrypt(b"hello", b"correct horse").unwrap();
    assert_eq!(
        decrypt(&encoded, b"correct horse").unwrap().as_slice(),
        b"hello"
    );
    assert_eq!(
        decrypt(&encoded, b"wrong").unwrap_err(),
        BackupError::Authentication
    );
    let mut envelope: Envelope = serde_json::from_slice(&encoded).unwrap();
    envelope.ciphertext[0] ^= 1;
    assert_eq!(
        decrypt(&serde_json::to_vec(&envelope).unwrap(), b"correct horse").unwrap_err(),
        BackupError::Authentication
    );
    let mut envelope: Envelope = serde_json::from_slice(&encoded).unwrap();
    envelope.wrapped_key[0] ^= 1;
    assert_eq!(
        decrypt(&serde_json::to_vec(&envelope).unwrap(), b"correct horse").unwrap_err(),
        BackupError::Authentication
    );
    let mut envelope: Envelope = serde_json::from_slice(&encoded).unwrap();
    envelope.salt[0] ^= 1;
    assert_eq!(
        decrypt(&serde_json::to_vec(&envelope).unwrap(), b"correct horse").unwrap_err(),
        BackupError::Authentication
    );
    let mut envelope: Envelope = serde_json::from_slice(&encoded).unwrap();
    envelope.payload_nonce[0] ^= 1;
    assert_eq!(
        decrypt(&serde_json::to_vec(&envelope).unwrap(), b"correct horse").unwrap_err(),
        BackupError::Authentication
    );
}

#[test]
fn encryption_uses_fresh_random_material() {
    let first = encrypt(b"same", b"passphrase").unwrap();
    let second = encrypt(b"same", b"passphrase").unwrap();
    let a: Envelope = serde_json::from_slice(&first).unwrap();
    let b: Envelope = serde_json::from_slice(&second).unwrap();
    assert_ne!(a.salt, b.salt);
    assert_ne!(a.wrapping_nonce, b.wrapping_nonce);
    assert_ne!(a.payload_nonce, b.payload_nonce);
    assert_eq!(decrypt(&second, b"passphrase").unwrap().as_slice(), b"same");
}

#[test]
fn rejects_unsupported_version_and_resource_exhaustion_before_kdf() {
    let encoded = encrypt(b"x", b"p").unwrap();
    let mut envelope: Envelope = serde_json::from_slice(&encoded).unwrap();
    envelope.version = 3;
    assert_eq!(
        decrypt(&serde_json::to_vec(&envelope).unwrap(), b"p").unwrap_err(),
        BackupError::UnsupportedVersion
    );
    envelope.version = 1;
    envelope.kdf.memory_kib = u32::MAX;
    assert_eq!(
        decrypt(&serde_json::to_vec(&envelope).unwrap(), b"p").unwrap_err(),
        BackupError::InvalidParameters
    );
    assert_eq!(
        encrypt(&vec![0; MAX_PAYLOAD_BYTES + 1], b"p").unwrap_err(),
        BackupError::PayloadTooLarge
    );
    assert_eq!(
        encrypt(b"x", b"").unwrap_err(),
        BackupError::InvalidPassword
    );
    assert_eq!(
        decrypt(b"not json", b"p").unwrap_err(),
        BackupError::InvalidEnvelope
    );
    envelope.kdf.memory_kib = 8192;
    envelope.kdf.iterations = u32::MAX;
    assert_eq!(
        decrypt(&serde_json::to_vec(&envelope).unwrap(), b"p").unwrap_err(),
        BackupError::InvalidParameters
    );
    envelope.kdf.iterations = 1;
    envelope.kdf.parallelism = 0;
    assert_eq!(
        decrypt(&serde_json::to_vec(&envelope).unwrap(), b"p").unwrap_err(),
        BackupError::InvalidParameters
    );
    assert_eq!(
        decrypt(&vec![b' '; 20 * 1024 * 1024 + 1], b"p").unwrap_err(),
        BackupError::PayloadTooLarge
    );
}

#[test]
fn empty_and_maximum_payloads_roundtrip() {
    for payload in [Vec::new(), vec![255; MAX_PAYLOAD_BYTES]] {
        let encoded = encrypt(&payload, b"passphrase").unwrap();
        assert_eq!(
            decrypt(&encoded, b"passphrase").unwrap().as_slice(),
            payload.as_slice()
        );
    }
}

#[test]
fn debug_output_never_exposes_keys_or_plaintext() {
    let secret = b"sensitive-plain-text";
    let encoded = encrypt(secret, b"sensitive-password").unwrap();
    let envelope: Envelope = serde_json::from_slice(&encoded).unwrap();
    let debug = format!("{envelope:?}");
    assert!(!debug.contains("sensitive"));
    assert!(!debug.contains("[4, 4, 4"));
    let plaintext = decrypt(&encoded, b"sensitive-password").unwrap();
    assert!(!format!("{plaintext:?}").contains("sensitive"));
    assert!(!format!("{plaintext:?}").contains("115, 101, 110"));
}
