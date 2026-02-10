use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use rand::RngCore;
use zeroize::Zeroizing;

use crate::format::FormatError;

/// Size of the AES-GCM nonce in bytes.
const NONCE_SIZE: usize = 12;

/// Size of the AES-GCM authentication tag in bytes.
const TAG_SIZE: usize = 16;

/// Size of the Argon2 salt in bytes.
pub const SALT_SIZE: usize = 16;

/// Generate a random 16-byte salt for key derivation.
pub fn generate_salt() -> [u8; SALT_SIZE] {
    let mut salt = [0u8; SALT_SIZE];
    rand::rng().fill_bytes(&mut salt);
    salt
}

/// Derive a 256-bit key from a passphrase and salt using Argon2id.
///
/// Uses the `argon2` crate v0.5 defaults (Argon2id variant):
///   - Memory cost: m=19456 (19 MiB)
///   - Time cost:   t=2 iterations
///   - Parallelism:  p=1
///
/// These parameters are fixed for `.egg` format version 1.
pub fn derive_key(
    passphrase: &str,
    salt: &[u8; SALT_SIZE],
) -> Result<Zeroizing<[u8; 32]>, FormatError> {
    let mut key = Zeroizing::new([0u8; 32]);
    argon2::Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, &mut *key)
        .map_err(|e| FormatError::Encryption(format!("key derivation failed: {e}")))?;
    Ok(key)
}

/// Encrypt a block using AES-256-GCM.
///
/// Returns `[12-byte nonce][ciphertext + 16-byte GCM tag]`.
pub fn encrypt_block(data: &[u8], key: &[u8; 32]) -> Result<Vec<u8>, FormatError> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| FormatError::Encryption(format!("cipher init failed: {e}")))?;

    let mut nonce_bytes = [0u8; NONCE_SIZE];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, data)
        .map_err(|e| FormatError::Encryption(format!("encryption failed: {e}")))?;

    let mut result = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    result.extend_from_slice(&nonce_bytes);
    result.extend_from_slice(&ciphertext);
    Ok(result)
}

/// Decrypt a block encrypted with AES-256-GCM.
///
/// Input format: `[12-byte nonce][ciphertext + 16-byte GCM tag]`.
pub fn decrypt_block(data: &[u8], key: &[u8; 32]) -> Result<Vec<u8>, FormatError> {
    if data.len() < NONCE_SIZE + TAG_SIZE {
        return Err(FormatError::Encryption("encrypted block too short".into()));
    }

    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| FormatError::Encryption(format!("cipher init failed: {e}")))?;

    let nonce = Nonce::from_slice(&data[..NONCE_SIZE]);
    let ciphertext = &data[NONCE_SIZE..];

    cipher.decrypt(nonce, ciphertext).map_err(|_| {
        FormatError::Encryption("decryption failed: wrong passphrase or corrupt data".into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_round_trip() {
        let key = [42u8; 32];
        let plaintext = b"hello, encryption world!";
        let encrypted = encrypt_block(plaintext, &key).unwrap();
        let decrypted = decrypt_block(&encrypted, &key).unwrap();
        assert_eq!(&decrypted, plaintext);
    }

    #[test]
    fn wrong_key_fails() {
        let key = [42u8; 32];
        let wrong_key = [99u8; 32];
        let plaintext = b"secret data";
        let encrypted = encrypt_block(plaintext, &key).unwrap();
        let result = decrypt_block(&encrypted, &wrong_key);
        assert!(result.is_err());
    }

    #[test]
    fn empty_data_round_trip() {
        let key = [1u8; 32];
        let encrypted = encrypt_block(b"", &key).unwrap();
        let decrypted = decrypt_block(&encrypted, &key).unwrap();
        assert!(decrypted.is_empty());
    }

    #[test]
    fn derive_key_consistent() {
        let salt = [0u8; SALT_SIZE];
        let key1 = derive_key("test-passphrase", &salt).unwrap();
        let key2 = derive_key("test-passphrase", &salt).unwrap();
        assert_eq!(key1, key2);
    }

    #[test]
    fn derive_key_different_passphrase() {
        let salt = [0u8; SALT_SIZE];
        let key1 = derive_key("pass1", &salt).unwrap();
        let key2 = derive_key("pass2", &salt).unwrap();
        assert_ne!(key1, key2);
    }

    #[test]
    fn derive_key_different_salt() {
        let salt1 = [0u8; SALT_SIZE];
        let salt2 = [1u8; SALT_SIZE];
        let key1 = derive_key("same-pass", &salt1).unwrap();
        let key2 = derive_key("same-pass", &salt2).unwrap();
        assert_ne!(key1, key2);
    }

    #[test]
    fn encrypted_block_too_short() {
        let key = [42u8; 32];
        let result = decrypt_block(&[0u8; 10], &key);
        assert!(result.is_err());
    }

    #[test]
    fn generate_salt_not_zero() {
        let salt = generate_salt();
        // Extremely unlikely to be all zeros
        assert!(salt.iter().any(|&b| b != 0));
    }

    #[test]
    fn large_data_round_trip() {
        let key = [77u8; 32];
        let plaintext: Vec<u8> = (0..10_000).map(|i| (i % 256) as u8).collect();
        let encrypted = encrypt_block(&plaintext, &key).unwrap();
        let decrypted = decrypt_block(&encrypted, &key).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn encrypted_output_larger_than_input() {
        let key = [42u8; 32];
        let plaintext = b"test";
        let encrypted = encrypt_block(plaintext, &key).unwrap();
        // Output = NONCE_SIZE(12) + plaintext(4) + TAG_SIZE(16) = 32
        assert_eq!(encrypted.len(), NONCE_SIZE + plaintext.len() + TAG_SIZE);
    }
}
