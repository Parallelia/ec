use base64::Engine;
use blind_rsa_signatures::{DefaultRng, PSS, PublicKey, Randomized, Sha384};
use ec::crypto;

#[test]
fn test_crypto_roundtrip() -> anyhow::Result<()> {
    // 1. Generate keypair (EC side)
    let (pk_der_b64, sk_der_b64) = crypto::generate_keypair()?;

    // 2. Load public key (Client side)
    let pk_der = base64::engine::general_purpose::STANDARD.decode(&pk_der_b64)?;
    let pk = PublicKey::<Sha384, PSS, Randomized>::from_der(&pk_der)?;

    // 3. Blind message (Client side)
    let mut rng = DefaultRng;
    let message = b"my secret nonce";
    let blinding_result = pk.blind(&mut rng, message)?;

    // 4. Blind sign (EC side)
    let blind_sig = crypto::blind_sign(&sk_der_b64, &blinding_result.blind_message)?;

    // 5. Finalize signature (Client side)
    let sig = pk.finalize(&blind_sig.into(), &blinding_result, message)?;

    // 6. Verify signature (EC side)
    // In Randomized mode, msg_randomizer is required.
    let randomizer = blinding_result
        .msg_randomizer
        .expect("Randomized mode must have a randomizer");

    crypto::verify_signature(&pk_der_b64, &sig, randomizer.as_ref(), message)?;

    Ok(())
}

#[test]
fn test_nonce_generation() {
    let n1 = crypto::generate_nonce();
    let n2 = crypto::generate_nonce();
    assert_ne!(n1, n2);
    assert_eq!(n1.len(), 32);
}

#[test]
fn blind_sign_rejects_invalid_base64_key() {
    assert!(crypto::blind_sign("not-base64!!!", b"blinded").is_err());
}

#[test]
fn blind_sign_rejects_invalid_der_key() {
    let bogus = base64::engine::general_purpose::STANDARD.encode(b"not a der key");
    assert!(crypto::blind_sign(&bogus, b"blinded").is_err());
}

#[test]
fn verify_signature_rejects_invalid_base64_key() {
    let randomizer = [0u8; 32];
    assert!(crypto::verify_signature("%%%", b"sig", &randomizer, b"msg").is_err());
}

#[test]
fn verify_signature_rejects_invalid_der_key() {
    let bogus = base64::engine::general_purpose::STANDARD.encode(b"not a der key");
    let randomizer = [0u8; 32];
    assert!(crypto::verify_signature(&bogus, b"sig", &randomizer, b"msg").is_err());
}

#[test]
fn verify_signature_rejects_wrong_randomizer_length() -> anyhow::Result<()> {
    let (pk_der_b64, _) = crypto::generate_keypair()?;
    let err = crypto::verify_signature(&pk_der_b64, b"sig", &[0u8; 16], b"msg")
        .expect_err("short randomizer must fail");
    assert!(
        err.to_string()
            .contains("Invalid message randomizer length")
    );
    Ok(())
}

#[test]
fn verify_signature_rejects_tampered_message() -> anyhow::Result<()> {
    let (pk_der_b64, sk_der_b64) = crypto::generate_keypair()?;

    let pk_der = base64::engine::general_purpose::STANDARD.decode(&pk_der_b64)?;
    let pk = PublicKey::<Sha384, PSS, Randomized>::from_der(&pk_der)?;

    let mut rng = DefaultRng;
    let message = b"legitimate nonce";
    let blinding_result = pk.blind(&mut rng, message)?;
    let blind_sig = crypto::blind_sign(&sk_der_b64, &blinding_result.blind_message)?;
    let sig = pk.finalize(&blind_sig.into(), &blinding_result, message)?;
    let randomizer = blinding_result.msg_randomizer.expect("randomizer");

    // Same signature, different message → must fail verification.
    assert!(crypto::verify_signature(&pk_der_b64, &sig, randomizer.as_ref(), b"tampered").is_err());
    Ok(())
}
