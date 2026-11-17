use coshell_backup::{create_backup_context, decrypt, BackupError, BackupKey, WrappedBackupKey};

#[test]
fn automatic_backup_survives_restart_and_password_recovers_each_snapshot() {
    let (context, key) = create_backup_context(b"passphrase").unwrap();
    let persisted_context = context.to_bytes().unwrap();
    let persisted_key = key.for_secret_store().to_vec();
    drop(context);
    drop(key);
    let context = WrappedBackupKey::from_bytes(&persisted_context).unwrap();
    let key = BackupKey::from_secret_store(&persisted_key).unwrap();
    let first = context.encrypt(b"snapshot one", &key).unwrap();
    let second = context.encrypt(b"snapshot two", &key).unwrap();
    let first_json: serde_json::Value = serde_json::from_slice(&first).unwrap();
    let second_json: serde_json::Value = serde_json::from_slice(&second).unwrap();
    assert_ne!(first_json["payload_nonce"], second_json["payload_nonce"]);
    assert_eq!(
        decrypt(&first, b"passphrase").unwrap().as_slice(),
        b"snapshot one"
    );
    assert_eq!(
        decrypt(&second, b"passphrase").unwrap().as_slice(),
        b"snapshot two"
    );
    assert_eq!(
        decrypt(&first, b"wrong").unwrap_err(),
        BackupError::Authentication
    );
    let mut modified = first_json;
    modified["ciphertext"][0] =
        serde_json::json!((modified["ciphertext"][0].as_u64().unwrap() ^ 1) as u8);
    assert_eq!(
        decrypt(&serde_json::to_vec(&modified).unwrap(), b"passphrase").unwrap_err(),
        BackupError::Authentication
    );
    let wrong = BackupKey::from_secret_store(&[99; 32]).unwrap();
    assert_eq!(
        context.encrypt(b"x", &wrong).unwrap_err(),
        BackupError::Authentication
    );
    assert_eq!(
        BackupKey::from_secret_store(&[1; 31]).err(),
        Some(BackupError::InvalidParameters)
    );
    let mut changed = persisted_context.clone();
    let position = changed.iter().position(|b| *b == b'1').unwrap();
    changed[position] = b'9';
    assert!(WrappedBackupKey::from_bytes(&changed)
        .and_then(|c| c.encrypt(b"x", &key))
        .is_err());
}
