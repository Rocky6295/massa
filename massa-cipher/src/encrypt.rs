// Copyright (c) 2022 MASSA LABS <info@massa.net>

use aes_gcm_siv::aead::{Aead, NewAead};
use aes_gcm_siv::{Aes256GcmSiv, Key, Nonce};
use pbkdf2::{
    password_hash::{PasswordHasher, SaltString},
    Pbkdf2,
};
use rand::{thread_rng, RngCore};
use rand_core::OsRng;

use crate::constants::NONCE_SIZE;
use crate::error::CipherError;

/// Encryption function using AES-GCM-SIV cipher.
///
/// Read `lib.rs` module documentation for more information.
pub fn encrypt(password: &str, data: &[u8]) -> Result<Vec<u8>, CipherError> {
    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Pbkdf2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| CipherError::EncryptionError(e.to_string()))?
        .hash
        .expect("content is missing after a successful hash");
    let cipher = Aes256GcmSiv::new(Key::from_slice(password_hash.as_bytes()));
    let mut nonce_bytes = [0u8; NONCE_SIZE];
    thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let encrypted_bytes = cipher
        .encrypt(nonce, data.as_ref())
        .map_err(|e| CipherError::EncryptionError(e.to_string()))?;
    let mut content = salt.as_bytes().to_vec();
    content.extend(nonce_bytes);
    content.extend(encrypted_bytes);
    Ok(content)
}
