//! Sign native release packages with the Minisign key the updater trusts.
//!
//! The native updater in `monocode-core` verifies every package with a
//! streaming Minisign verifier, so the signatures must be the prehashed
//! ("ED") variant this crate writes. The signature file holds the base64 of
//! the Minisign signature text, which is the form the update feed carries.

use std::{env, fs::File, path::Path};

use base64::{engine::general_purpose::STANDARD, Engine};

/// A signing key and the public key that must accept its signatures.
pub struct Signer {
    secret: minisign::SecretKey,
    public: Option<minisign::PublicKey>,
}

impl Signer {
    /// Read the signing key from the environment, base64 encoded like the
    /// public key the updater is configured with.
    pub fn from_env() -> Result<Self, String> {
        let raw = env::var("MONOCODE_NATIVE_SIGNING_PRIVATE_KEY")
            .or_else(|_| env::var("TAURI_SIGNING_PRIVATE_KEY"))
            .map_err(|_| "Missing the native signing private key".to_string())?;
        let password = env::var("MONOCODE_NATIVE_SIGNING_KEY_PASSWORD")
            .or_else(|_| env::var("TAURI_SIGNING_PRIVATE_KEY_PASSWORD"))
            .ok()
            .filter(|password| !password.is_empty());
        let public = env::var("MONOCODE_NATIVE_UPDATER_PUBKEY")
            .or_else(|_| env::var("TAURI_UPDATER_PUBKEY"))
            .ok()
            .filter(|public| !public.is_empty());
        Self::from_keys(&raw, password.as_deref(), public.as_deref())
    }

    /// Build a signer from the key files, raw or base64 encoded.
    pub fn from_keys(
        secret: &str,
        password: Option<&str>,
        public: Option<&str>,
    ) -> Result<Self, String> {
        let box_ = minisign::SecretKeyBox::from_string(&key_text(secret))
            .map_err(|e| format!("Invalid native signing private key: {e}"))?;
        let secret = match minisign::SecretKey::from_unencrypted_box(box_.clone()) {
            Ok(secret) => secret,
            Err(_) => {
                let password = password
                    .ok_or("The native signing private key is encrypted but has no password")?;
                minisign::SecretKey::from_box(box_, Some(password.to_string()))
                    .map_err(|e| format!("Could not unlock the native signing private key: {e}"))?
            }
        };
        let public = public.and_then(|public| {
            minisign::PublicKeyBox::from_string(&key_text(public))
                .ok()?
                .into_public_key()
                .ok()
        });
        Ok(Self { secret, public })
    }

    /// Sign a file and write the base64 of its Minisign signature next to it.
    pub fn sign_file(&self, path: &Path) -> Result<(), String> {
        let signature = minisign::sign(
            None,
            &self.secret,
            File::open(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?,
            None,
            Some("signature from monocode native release"),
        )
        .map_err(|e| format!("Could not sign {}: {e}", path.display()))?;
        if let Some(public) = &self.public {
            minisign::verify(
                public,
                &signature,
                File::open(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?,
                true,
                false,
                false,
            )
            .map_err(|e| format!("The signature does not match the update public key: {e}"))?;
        }
        let text = signature.to_string();
        let encoded = STANDARD.encode(text.as_bytes());
        let output = format!("{}.sig", path.display());
        std::fs::write(&output, encoded).map_err(|e| format!("Could not write {output}: {e}"))
    }
}

/// Keys are stored base64 encoded, but a plain key file also works.
fn key_text(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Ok(bytes) = STANDARD.decode(trimmed) {
        if let Ok(text) = String::from_utf8(bytes) {
            if text.contains("untrusted comment:") {
                return text;
            }
        }
    }
    trimmed.to_string()
}

/// Write a new signing key pair, base64 encoded like the keys the updater
/// and the release job use.
pub fn generate_keys(path: &Path, password: Option<&str>) -> Result<(String, String), String> {
    let keypair = minisign::KeyPair::generate_encrypted_keypair(password.map(str::to_string))
        .map_err(|e| format!("Could not generate a signing key: {e}"))?;
    let secret = keypair
        .sk
        .to_box(Some("monocode native signing key"))
        .map_err(|e| format!("Could not write the signing key: {e}"))?
        .into_string();
    let public = keypair
        .pk
        .to_box()
        .map_err(|e| format!("Could not write the update public key: {e}"))?
        .into_string();
    let secret = STANDARD.encode(secret.as_bytes());
    let public = STANDARD.encode(public.as_bytes());
    std::fs::write(path, &secret)
        .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    let public_path = path.with_extension("pub");
    std::fs::write(&public_path, &public)
        .map_err(|e| format!("Could not write {}: {e}", public_path.display()))?;
    Ok((secret, public))
}
