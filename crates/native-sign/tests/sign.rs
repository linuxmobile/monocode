//! The signatures must verify with the updater's own verifier.

use std::{fs, path::Path};

use base64::{engine::general_purpose::STANDARD, Engine};

/// The updater reads the public key and the signature as base64 of the
/// Minisign files, and verifies prehashed signatures while streaming.
fn verifier_key(value: &str) -> minisign_verify::PublicKey {
    let text = String::from_utf8(STANDARD.decode(value.trim()).unwrap()).unwrap();
    minisign_verify::PublicKey::decode(&text).unwrap()
}

fn verifier_signature(value: &str) -> minisign_verify::Signature {
    let text = String::from_utf8(STANDARD.decode(value.trim()).unwrap()).unwrap();
    minisign_verify::Signature::decode(&text).unwrap()
}

fn verify_file(key: &minisign_verify::PublicKey, path: &Path) {
    let signature =
        verifier_signature(&fs::read_to_string(format!("{}.sig", path.display())).unwrap());
    let mut verifier = key.verify_stream(&signature).unwrap();
    let bytes = fs::read(path).unwrap();
    verifier.update(&bytes);
    verifier.finalize().unwrap();
}

#[test]
fn signed_packages_verify_with_the_updater_verifier() {
    let keypair = minisign::KeyPair::generate_encrypted_keypair(Some("password".into())).unwrap();
    let secret = keypair
        .sk
        .to_box(Some("monocode test key"))
        .unwrap()
        .into_string();
    let public = keypair.pk.to_box().unwrap().into_string();
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("package.bin");
    fs::write(&package, b"native package contents").unwrap();

    let signer = native_sign::Signer::from_keys(
        &STANDARD.encode(secret.as_bytes()),
        Some("password"),
        Some(&STANDARD.encode(public.as_bytes())),
    )
    .unwrap();
    signer.sign_file(&package).unwrap();

    verify_file(&verifier_key(&STANDARD.encode(public.as_bytes())), &package);
}

#[test]
fn a_wrong_password_cannot_sign() {
    let keypair = minisign::KeyPair::generate_encrypted_keypair(Some("password".into())).unwrap();
    let secret = keypair
        .sk
        .to_box(Some("monocode test key"))
        .unwrap()
        .into_string();
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("package.bin");
    fs::write(&package, b"native package contents").unwrap();

    assert!(native_sign::Signer::from_keys(
        &STANDARD.encode(secret.as_bytes()),
        Some("wrong"),
        None
    )
    .is_err());
}
