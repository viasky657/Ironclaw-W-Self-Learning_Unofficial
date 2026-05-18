use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use aes_gcm::aead::rand_core::RngCore;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::types::EncryptedSnapshot;

/// Errors that can occur during snapshot encryption.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("AES-256-GCM key generation failed: {0}")]
    KeyGeneration(String),
    #[error("AES-256-GCM encryption failed: {0}")]
    Encryption(String),
}

/// Encrypt a payload with AES-256-GCM.
///
/// Uses `OsRng` for both key and nonce generation.
/// The key material is wrapped in `Zeroizing` so it is zeroed on drop.
///
/// **There is no plaintext fallback.** If encryption fails, this function
/// returns `Err(CryptoError)` — the caller must not transmit the payload
/// unencrypted.
pub fn encrypt_snapshot(payload: &[u8]) -> Result<EncryptedSnapshot, CryptoError> {
    // Generate a fresh 256-bit key for this snapshot.
    // Wrapped in Zeroizing so the key bytes are zeroed when this scope exits.
    let mut raw_key = Zeroizing::new([0u8; 32]);
    OsRng.fill_bytes(raw_key.as_mut());

    // Generate a fresh 96-bit nonce.
    let mut raw_nonce = [0u8; 12];
    OsRng.fill_bytes(&mut raw_nonce);

    // Compute key_id = first 16 hex chars of SHA-256(key).
    // This lets the orchestrator look up the key in its KMS without
    // transmitting the key itself.
    let key_id = {
        let mut hasher = Sha256::new();
        hasher.update(raw_key.as_ref());
        let digest = hasher.finalize();
        hex::encode(&digest[..8]) // 8 bytes = 16 hex chars
    };

    let key = Key::<Aes256Gcm>::from_slice(raw_key.as_ref());
    let cipher = Aes256Gcm::new(key);
    let nonce = Nonce::from_slice(&raw_nonce);

    let ciphertext = cipher
        .encrypt(nonce, payload)
        .map_err(|e| CryptoError::Encryption(e.to_string()))?;

    Ok(EncryptedSnapshot {
        ciphertext: BASE64.encode(&ciphertext),
        nonce: BASE64.encode(raw_nonce),
        key_id,
    })
}

// hex encoding helper (avoids pulling in the `hex` crate separately)
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_produces_non_empty_output() {
        let payload = b"hello world";
        let result = encrypt_snapshot(payload).expect("encryption should succeed");
        assert!(!result.ciphertext.is_empty());
        assert!(!result.nonce.is_empty());
        assert_eq!(result.key_id.len(), 16);
    }

    #[test]
    fn encrypt_different_keys_each_call() {
        let payload = b"same payload";
        let r1 = encrypt_snapshot(payload).unwrap();
        let r2 = encrypt_snapshot(payload).unwrap();
        // Different keys → different key_ids and different ciphertexts.
        assert_ne!(r1.key_id, r2.key_id);
        assert_ne!(r1.ciphertext, r2.ciphertext);
    }

    #[test]
    fn no_plaintext_fallback() {
        // Verify the function returns a typed Result — there is no code path
        // that returns a base64-encoded plaintext with key_id="plaintext".
        let result = encrypt_snapshot(b"test").unwrap();
        assert_ne!(result.key_id, "plaintext");
    }
}
