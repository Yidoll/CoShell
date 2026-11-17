//! Authenticated, versioned connection backups. Restore decisions belong in core.
pub mod snapshot_manifest;
pub mod webdav;

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    aead::{rand_core::RngCore, Aead, KeyInit, OsRng, Payload},
    XChaCha20Poly1305, XNonce,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

pub const MAX_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;
const MAX_ENVELOPE_BYTES: usize = 20 * 1024 * 1024;
const MAX_PASSWORD_BYTES: usize = 1024;
const WRAP_CONTEXT: &[u8] = b"coshell-backup-v1:key-wrap";
const DATA_CONTEXT: &[u8] = b"coshell-backup-v1:payload";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupError {
    InvalidInput,
    InvalidPassword,
    PayloadTooLarge,
    InvalidEnvelope,
    UnsupportedVersion,
    InvalidParameters,
    Authentication,
    RandomUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KdfParams {
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        Self {
            memory_kib: 19_456,
            iterations: 2,
            parallelism: 1,
        }
    }
}

impl KdfParams {
    fn validate(self) -> Result<(), BackupError> {
        // Bounds are checked before Argon2 allocates memory or spends CPU.
        if !(8_192..=65_536).contains(&self.memory_kib)
            || !(1..=4).contains(&self.iterations)
            || !(1..=4).contains(&self.parallelism)
            || self.memory_kib < self.parallelism.saturating_mul(8)
        {
            return Err(BackupError::InvalidParameters);
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub version: u32,
    pub kdf: KdfParams,
    pub salt: [u8; 16],
    pub wrapping_nonce: [u8; 24],
    pub payload_nonce: [u8; 24],
    pub wrapped_key: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

impl std::fmt::Debug for Envelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Envelope")
            .field("version", &self.version)
            .field("kdf", &self.kdf)
            .field("wrapped_key", &"[REDACTED]")
            .field("ciphertext", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

struct Material {
    salt: [u8; 16],
    wrapping_nonce: [u8; 24],
    payload_nonce: [u8; 24],
    data_key: Zeroizing<[u8; 32]>,
}

/// Decrypted bytes are zeroized on drop and are never exposed through Debug.
pub struct Plaintext(Zeroizing<Vec<u8>>);

impl Plaintext {
    pub fn as_slice(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl std::fmt::Debug for Plaintext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Plaintext([REDACTED])")
    }
}

fn password_key(
    password: &[u8],
    salt: &[u8; 16],
    params: KdfParams,
) -> Result<Zeroizing<[u8; 32]>, BackupError> {
    if password.is_empty() || password.len() > MAX_PASSWORD_BYTES {
        return Err(BackupError::InvalidPassword);
    }
    params.validate()?;
    let argon_params = Params::new(
        params.memory_kib,
        params.iterations,
        params.parallelism,
        Some(32),
    )
    .map_err(|_| BackupError::InvalidParameters)?;
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, argon_params)
        .hash_password_into(password, salt, key.as_mut())
        .map_err(|_| BackupError::InvalidParameters)?;
    Ok(key)
}

fn aad(
    context: &[u8],
    version: u32,
    params: KdfParams,
    salt: &[u8; 16],
    wrapping_nonce: &[u8; 24],
    payload_nonce: &[u8; 24],
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(context.len() + 80);
    bytes.extend_from_slice(context);
    for value in [
        version,
        params.memory_kib,
        params.iterations,
        params.parallelism,
    ] {
        bytes.extend_from_slice(&value.to_be_bytes());
    }
    bytes.extend_from_slice(salt);
    bytes.extend_from_slice(wrapping_nonce);
    bytes.extend_from_slice(payload_nonce);
    bytes
}

fn encrypt_with_material(
    plaintext: &[u8],
    password: &[u8],
    params: KdfParams,
    material: Material,
) -> Result<Vec<u8>, BackupError> {
    if plaintext.len() > MAX_PAYLOAD_BYTES {
        return Err(BackupError::PayloadTooLarge);
    }
    let key = password_key(password, &material.salt, params)?;
    let wrap = XChaCha20Poly1305::new_from_slice(key.as_ref())
        .map_err(|_| BackupError::InvalidParameters)?;
    let wrap_aad = aad(
        WRAP_CONTEXT,
        1,
        params,
        &material.salt,
        &material.wrapping_nonce,
        &material.payload_nonce,
    );
    let wrapped_key = wrap
        .encrypt(
            XNonce::from_slice(&material.wrapping_nonce),
            Payload {
                msg: material.data_key.as_ref(),
                aad: &wrap_aad,
            },
        )
        .map_err(|_| BackupError::Authentication)?;
    let cipher = XChaCha20Poly1305::new_from_slice(material.data_key.as_ref())
        .map_err(|_| BackupError::InvalidParameters)?;
    let data_aad = aad(
        DATA_CONTEXT,
        1,
        params,
        &material.salt,
        &material.wrapping_nonce,
        &material.payload_nonce,
    );
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&material.payload_nonce),
            Payload {
                msg: plaintext,
                aad: &data_aad,
            },
        )
        .map_err(|_| BackupError::Authentication)?;
    serde_json::to_vec(&Envelope {
        version: 1,
        kdf: params,
        salt: material.salt,
        wrapping_nonce: material.wrapping_nonce,
        payload_nonce: material.payload_nonce,
        wrapped_key,
        ciphertext,
    })
    .map_err(|_| BackupError::InvalidEnvelope)
}

pub fn encrypt(plaintext: &[u8], password: &[u8]) -> Result<Vec<u8>, BackupError> {
    if plaintext.len() > MAX_PAYLOAD_BYTES {
        return Err(BackupError::PayloadTooLarge);
    }
    if password.is_empty() || password.len() > MAX_PASSWORD_BYTES {
        return Err(BackupError::InvalidPassword);
    }
    let mut material = Material {
        salt: [0; 16],
        wrapping_nonce: [0; 24],
        payload_nonce: [0; 24],
        data_key: Zeroizing::new([0; 32]),
    };
    OsRng
        .try_fill_bytes(&mut material.salt)
        .map_err(|_| BackupError::RandomUnavailable)?;
    OsRng
        .try_fill_bytes(&mut material.wrapping_nonce)
        .map_err(|_| BackupError::RandomUnavailable)?;
    OsRng
        .try_fill_bytes(&mut material.payload_nonce)
        .map_err(|_| BackupError::RandomUnavailable)?;
    OsRng
        .try_fill_bytes(material.data_key.as_mut())
        .map_err(|_| BackupError::RandomUnavailable)?;
    encrypt_with_material(plaintext, password, KdfParams::default(), material)
}

/// Version 3 is explicitly unencrypted and its checksum detects accidental
/// corruption only; it does not authenticate content or protect secrets.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnencryptedEnvelope {
    version: u32,
    protection: String,
    payload: SensitiveString,
    sha256: [u8; 32],
}

/// Owns decoded sensitive text as soon as serde hands it to us, including if
/// parsing a later field fails. Never implements Debug or serialization.
struct SensitiveString(Zeroizing<String>);
impl<'de> Deserialize<'de> for SensitiveString {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = SensitiveString;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a bounded UTF-8 payload")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                if value.len() > MAX_PAYLOAD_BYTES {
                    return Err(E::custom("payload too large"));
                }
                Ok(SensitiveString(Zeroizing::new(value.to_owned())))
            }
            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
                let value = Zeroizing::new(value);
                if value.len() > MAX_PAYLOAD_BYTES {
                    return Err(E::custom("payload too large"));
                }
                Ok(SensitiveString(value))
            }
        }
        deserializer.deserialize_string(Visitor)
    }
}

#[derive(Serialize)]
struct UnencryptedEnvelopeRef<'a> {
    version: u32,
    protection: &'static str,
    payload: &'a str,
    sha256: [u8; 32],
}

pub fn encode_optional(plaintext: &[u8], password: &[u8]) -> Result<Vec<u8>, BackupError> {
    if !password.is_empty() {
        return encrypt(plaintext, password);
    }
    if plaintext.len() > MAX_PAYLOAD_BYTES {
        return Err(BackupError::PayloadTooLarge);
    }
    let payload = std::str::from_utf8(plaintext).map_err(|_| BackupError::InvalidEnvelope)?;
    serde_json::to_vec(&UnencryptedEnvelopeRef {
        version: 3,
        protection: "none",
        payload,
        sha256: Sha256::digest(plaintext).into(),
    })
    .map_err(|_| BackupError::InvalidEnvelope)
}

pub fn decode_optional(encoded: &[u8], password: &[u8]) -> Result<Plaintext, BackupError> {
    if encoded.len() > MAX_ENVELOPE_BYTES {
        return Err(BackupError::PayloadTooLarge);
    }
    if probe_version(encoded)? != 3 {
        return decrypt(encoded, password);
    }
    if !password.is_empty() {
        return Err(BackupError::InvalidPassword);
    }
    let envelope: UnencryptedEnvelope =
        serde_json::from_slice(encoded).map_err(|_| BackupError::InvalidEnvelope)?;
    if envelope.version != 3
        || envelope.protection != "none"
        || envelope.payload.0.len() > MAX_PAYLOAD_BYTES
    {
        return Err(BackupError::InvalidEnvelope);
    }
    if <[u8; 32]>::from(Sha256::digest(envelope.payload.0.as_bytes())) != envelope.sha256 {
        return Err(BackupError::Authentication);
    }
    let mut payload = envelope.payload.0;
    Ok(Plaintext(Zeroizing::new(
        std::mem::take(&mut *payload).into_bytes(),
    )))
}

#[derive(Deserialize)]
struct VersionProbe {
    version: u32,
}

fn probe_version(encoded: &[u8]) -> Result<u32, BackupError> {
    // serde skips all other fields with IgnoredAny without constructing a Value tree.
    let header: VersionProbe =
        serde_json::from_slice(encoded).map_err(|_| BackupError::InvalidEnvelope)?;
    Ok(header.version)
}

pub fn protection_mode(encoded: &[u8]) -> Result<&'static str, BackupError> {
    if encoded.len() > MAX_ENVELOPE_BYTES {
        return Err(BackupError::PayloadTooLarge);
    }
    match probe_version(encoded)? {
        1 | 2 => Ok("encrypted"),
        3 => Ok("none"),
        _ => Err(BackupError::UnsupportedVersion),
    }
}

pub fn decrypt(encoded: &[u8], password: &[u8]) -> Result<Plaintext, BackupError> {
    if encoded.len() > MAX_ENVELOPE_BYTES {
        return Err(BackupError::PayloadTooLarge);
    }
    if probe_version(encoded)? == 2 {
        return decrypt_v2(encoded, password);
    }
    let envelope: Envelope =
        serde_json::from_slice(encoded).map_err(|_| BackupError::InvalidEnvelope)?;
    if envelope.version != 1 {
        return Err(BackupError::UnsupportedVersion);
    }
    envelope.kdf.validate()?;
    if envelope.wrapped_key.len() != 48
        || envelope.ciphertext.len() < 16
        || envelope.ciphertext.len() > MAX_PAYLOAD_BYTES + 16
    {
        return Err(BackupError::InvalidEnvelope);
    }
    let key = password_key(password, &envelope.salt, envelope.kdf)?;
    let wrap = XChaCha20Poly1305::new_from_slice(key.as_ref())
        .map_err(|_| BackupError::InvalidParameters)?;
    let wrap_aad = aad(
        WRAP_CONTEXT,
        1,
        envelope.kdf,
        &envelope.salt,
        &envelope.wrapping_nonce,
        &envelope.payload_nonce,
    );
    let mut data_key = Zeroizing::new(
        wrap.decrypt(
            XNonce::from_slice(&envelope.wrapping_nonce),
            Payload {
                msg: &envelope.wrapped_key,
                aad: &wrap_aad,
            },
        )
        .map_err(|_| BackupError::Authentication)?,
    );
    if data_key.len() != 32 {
        data_key.zeroize();
        return Err(BackupError::InvalidEnvelope);
    }
    let cipher = XChaCha20Poly1305::new_from_slice(data_key.as_slice())
        .map_err(|_| BackupError::InvalidEnvelope)?;
    let data_aad = aad(
        DATA_CONTEXT,
        1,
        envelope.kdf,
        &envelope.salt,
        &envelope.wrapping_nonce,
        &envelope.payload_nonce,
    );
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(&envelope.payload_nonce),
            Payload {
                msg: &envelope.ciphertext,
                aad: &data_aad,
            },
        )
        .map_err(|_| BackupError::Authentication)?;
    Ok(Plaintext(Zeroizing::new(plaintext)))
}

const V2_WRAP: &[u8] = b"coshell-backup-v2:key-wrap";
const V2_CHECK: &[u8] = b"coshell-backup-v2:key-check";
const V2_DATA: &[u8] = b"coshell-backup-v2:payload";
const KEY_CHECK: &[u8] = b"coshell-backup-key-check-v2";

/// Secret key material for storage exclusively in the host's secure SecretStore.
/// This type is deliberately not serializable, clonable, or printable.
pub struct BackupKey(Zeroizing<[u8; 32]>);

impl BackupKey {
    pub fn from_secret_store(secret: &[u8]) -> Result<Self, BackupError> {
        if secret.len() != 32 {
            return Err(BackupError::InvalidParameters);
        }
        let mut bytes = Zeroizing::new([0u8; 32]);
        bytes.copy_from_slice(secret);
        Ok(Self(bytes))
    }

    /// Borrow briefly for direct transfer to SecretStore; never send to UI or logs.
    pub fn for_secret_store(&self) -> &[u8] {
        self.0.as_ref()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WrappedBackupKey {
    version: u32,
    kdf: KdfParams,
    salt: [u8; 16],
    wrapping_nonce: [u8; 24],
    wrapped_key: Vec<u8>,
    check_nonce: [u8; 24],
    key_check: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotV2 {
    version: u32,
    context: WrappedBackupKey,
    payload_nonce: [u8; 24],
    ciphertext: Vec<u8>,
}

fn v2_header(
    context: &[u8],
    version: u32,
    kdf: KdfParams,
    salt: &[u8; 16],
    nonce: &[u8; 24],
) -> Vec<u8> {
    let mut out = Vec::from(context);
    for value in [version, kdf.memory_kib, kdf.iterations, kdf.parallelism] {
        out.extend_from_slice(&value.to_be_bytes());
    }
    out.extend_from_slice(salt);
    out.extend_from_slice(nonce);
    out
}

impl WrappedBackupKey {
    fn validate(&self) -> Result<(), BackupError> {
        if self.version != 2 {
            return Err(BackupError::UnsupportedVersion);
        }
        self.kdf.validate()?;
        if self.wrapped_key.len() != 48 || self.key_check.len() != KEY_CHECK.len() + 16 {
            return Err(BackupError::InvalidEnvelope);
        }
        Ok(())
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, BackupError> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|_| BackupError::InvalidEnvelope)
    }

    pub fn from_bytes(encoded: &[u8]) -> Result<Self, BackupError> {
        if encoded.len() > 4096 {
            return Err(BackupError::InvalidEnvelope);
        }
        let context: Self =
            serde_json::from_slice(encoded).map_err(|_| BackupError::InvalidEnvelope)?;
        context.validate()?;
        Ok(context)
    }

    fn check_aad(&self) -> Vec<u8> {
        let mut out = v2_header(
            V2_CHECK,
            self.version,
            self.kdf,
            &self.salt,
            &self.wrapping_nonce,
        );
        out.extend_from_slice(&self.wrapped_key);
        out
    }

    fn verify_key(&self, key: &BackupKey) -> Result<(), BackupError> {
        self.validate()?;
        let cipher = XChaCha20Poly1305::new_from_slice(key.0.as_ref())
            .map_err(|_| BackupError::InvalidParameters)?;
        let clear = Zeroizing::new(
            cipher
                .decrypt(
                    XNonce::from_slice(&self.check_nonce),
                    Payload {
                        msg: &self.key_check,
                        aad: &self.check_aad(),
                    },
                )
                .map_err(|_| BackupError::Authentication)?,
        );
        if clear.as_slice() != KEY_CHECK {
            return Err(BackupError::Authentication);
        }
        Ok(())
    }

    pub fn unlock(&self, password: &[u8]) -> Result<BackupKey, BackupError> {
        self.validate()?;
        let derived = password_key(password, &self.salt, self.kdf)?;
        let cipher = XChaCha20Poly1305::new_from_slice(derived.as_ref())
            .map_err(|_| BackupError::InvalidParameters)?;
        let aad = v2_header(
            V2_WRAP,
            self.version,
            self.kdf,
            &self.salt,
            &self.wrapping_nonce,
        );
        let clear = Zeroizing::new(
            cipher
                .decrypt(
                    XNonce::from_slice(&self.wrapping_nonce),
                    Payload {
                        msg: &self.wrapped_key,
                        aad: &aad,
                    },
                )
                .map_err(|_| BackupError::Authentication)?,
        );
        let key = BackupKey::from_secret_store(clear.as_slice())?;
        self.verify_key(&key)?;
        Ok(key)
    }

    pub fn encrypt(&self, plaintext: &[u8], key: &BackupKey) -> Result<Vec<u8>, BackupError> {
        if plaintext.len() > MAX_PAYLOAD_BYTES {
            return Err(BackupError::PayloadTooLarge);
        }
        let mut nonce = [0u8; 24];
        OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(|_| BackupError::RandomUnavailable)?;
        self.encrypt_with_nonce(plaintext, key, nonce)
    }

    fn encrypt_with_nonce(
        &self,
        plaintext: &[u8],
        key: &BackupKey,
        nonce: [u8; 24],
    ) -> Result<Vec<u8>, BackupError> {
        if plaintext.len() > MAX_PAYLOAD_BYTES {
            return Err(BackupError::PayloadTooLarge);
        }
        self.verify_key(key)?;
        let mut aad = self.check_aad();
        aad.extend_from_slice(V2_DATA);
        aad.extend_from_slice(&self.check_nonce);
        aad.extend_from_slice(&self.key_check);
        let cipher = XChaCha20Poly1305::new_from_slice(key.0.as_ref())
            .map_err(|_| BackupError::InvalidParameters)?;
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| BackupError::Authentication)?;
        serde_json::to_vec(&SnapshotV2 {
            version: 2,
            context: self.clone(),
            payload_nonce: nonce,
            ciphertext,
        })
        .map_err(|_| BackupError::InvalidEnvelope)
    }
}

pub fn create_backup_context(
    password: &[u8],
) -> Result<(WrappedBackupKey, BackupKey), BackupError> {
    let mut salt = [0; 16];
    let mut wrapping_nonce = [0; 24];
    let mut check_nonce = [0; 24];
    let mut key = BackupKey(Zeroizing::new([0; 32]));
    OsRng
        .try_fill_bytes(&mut salt)
        .map_err(|_| BackupError::RandomUnavailable)?;
    OsRng
        .try_fill_bytes(&mut wrapping_nonce)
        .map_err(|_| BackupError::RandomUnavailable)?;
    OsRng
        .try_fill_bytes(&mut check_nonce)
        .map_err(|_| BackupError::RandomUnavailable)?;
    OsRng
        .try_fill_bytes(key.0.as_mut())
        .map_err(|_| BackupError::RandomUnavailable)?;
    create_context_with_material(
        password,
        KdfParams::default(),
        salt,
        wrapping_nonce,
        check_nonce,
        key,
    )
}

fn create_context_with_material(
    password: &[u8],
    kdf: KdfParams,
    salt: [u8; 16],
    wrapping_nonce: [u8; 24],
    check_nonce: [u8; 24],
    key: BackupKey,
) -> Result<(WrappedBackupKey, BackupKey), BackupError> {
    let derived = password_key(password, &salt, kdf)?;
    let wrap = XChaCha20Poly1305::new_from_slice(derived.as_ref())
        .map_err(|_| BackupError::InvalidParameters)?;
    let wrap_aad = v2_header(V2_WRAP, 2, kdf, &salt, &wrapping_nonce);
    let wrapped_key = wrap
        .encrypt(
            XNonce::from_slice(&wrapping_nonce),
            Payload {
                msg: key.0.as_ref(),
                aad: &wrap_aad,
            },
        )
        .map_err(|_| BackupError::Authentication)?;
    let mut context = WrappedBackupKey {
        version: 2,
        kdf,
        salt,
        wrapping_nonce,
        wrapped_key,
        check_nonce,
        key_check: Vec::new(),
    };
    let cipher = XChaCha20Poly1305::new_from_slice(key.0.as_ref())
        .map_err(|_| BackupError::InvalidParameters)?;
    context.key_check = cipher
        .encrypt(
            XNonce::from_slice(&check_nonce),
            Payload {
                msg: KEY_CHECK,
                aad: &context.check_aad(),
            },
        )
        .map_err(|_| BackupError::Authentication)?;
    Ok((context, key))
}

fn decrypt_v2(encoded: &[u8], password: &[u8]) -> Result<Plaintext, BackupError> {
    let snapshot: SnapshotV2 =
        serde_json::from_slice(encoded).map_err(|_| BackupError::InvalidEnvelope)?;
    if snapshot.version != 2 {
        return Err(BackupError::UnsupportedVersion);
    }
    if snapshot.ciphertext.len() < 16 || snapshot.ciphertext.len() > MAX_PAYLOAD_BYTES + 16 {
        return Err(BackupError::InvalidEnvelope);
    }
    let key = snapshot.context.unlock(password)?;
    let mut aad = snapshot.context.check_aad();
    aad.extend_from_slice(V2_DATA);
    aad.extend_from_slice(&snapshot.context.check_nonce);
    aad.extend_from_slice(&snapshot.context.key_check);
    let cipher = XChaCha20Poly1305::new_from_slice(key.0.as_ref())
        .map_err(|_| BackupError::InvalidParameters)?;
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(&snapshot.payload_nonce),
            Payload {
                msg: &snapshot.ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| BackupError::Authentication)?;
    Ok(Plaintext(Zeroizing::new(plaintext)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_v2_vector() {
        let (context, key) = create_context_with_material(
            b"correct horse",
            KdfParams {
                memory_kib: 8192,
                iterations: 1,
                parallelism: 1,
            },
            [1; 16],
            [2; 24],
            [3; 24],
            BackupKey(Zeroizing::new([4; 32])),
        )
        .unwrap();
        let snapshot: SnapshotV2 =
            serde_json::from_slice(&context.encrypt_with_nonce(b"hello", &key, [5; 24]).unwrap())
                .unwrap();
        assert_eq!(
            context.wrapped_key,
            vec![
                83, 14, 124, 24, 141, 97, 135, 187, 136, 72, 18, 202, 71, 241, 209, 227, 25, 132,
                207, 141, 11, 188, 120, 100, 83, 156, 250, 208, 168, 54, 243, 239, 210, 45, 175,
                41, 52, 4, 186, 213, 153, 239, 35, 197, 114, 109, 229, 194
            ]
        );
        assert_eq!(
            context.key_check,
            vec![
                25, 166, 252, 196, 181, 251, 154, 167, 201, 15, 75, 171, 154, 102, 191, 42, 86,
                100, 41, 6, 222, 89, 21, 64, 227, 15, 170, 25, 73, 151, 226, 39, 61, 129, 83, 203,
                34, 240, 220, 100, 138, 97, 103
            ]
        );
        assert_eq!(
            snapshot.ciphertext,
            vec![
                252, 102, 115, 93, 171, 186, 15, 203, 36, 95, 210, 115, 254, 119, 51, 33, 130, 130,
                2, 173, 41
            ]
        );
    }

    #[test]
    fn version_probe_skips_large_untrusted_arrays_and_rejects_duplicates() {
        let mut input = b"{\"version\":2,\"ciphertext\":[".to_vec();
        input.extend(
            std::iter::repeat(b"255,".as_slice())
                .take(1_000_000)
                .flatten()
                .copied(),
        );
        input.extend_from_slice(b"0]}");
        assert_eq!(probe_version(&input).unwrap(), 2);
        assert_eq!(
            probe_version(br#"{"version":1,"version":2}"#),
            Err(BackupError::InvalidEnvelope)
        );
    }

    #[test]
    fn fixed_material_vector() {
        let material = Material {
            salt: [1; 16],
            wrapping_nonce: [2; 24],
            payload_nonce: [3; 24],
            data_key: Zeroizing::new([4; 32]),
        };
        let encoded = encrypt_with_material(
            b"hello",
            b"correct horse",
            KdfParams {
                memory_kib: 8192,
                iterations: 1,
                parallelism: 1,
            },
            material,
        )
        .unwrap();
        let envelope: Envelope = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(
            envelope.wrapped_key,
            vec![
                83, 14, 124, 24, 141, 97, 135, 187, 136, 72, 18, 202, 71, 241, 209, 227, 25, 132,
                207, 141, 11, 188, 120, 100, 83, 156, 250, 208, 168, 54, 243, 239, 48, 97, 10, 229,
                187, 254, 97, 100, 166, 215, 66, 217, 160, 48, 62, 127
            ]
        );
        assert_eq!(
            envelope.ciphertext,
            vec![
                18, 172, 227, 192, 191, 243, 233, 169, 168, 114, 46, 176, 134, 131, 3, 231, 49,
                155, 108, 226, 148
            ]
        );
        assert_eq!(
            decrypt(&encoded, b"correct horse").unwrap().as_slice(),
            b"hello"
        );
    }
}
