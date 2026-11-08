//! Ed25519 key loading and FIX Logon signing.

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use ed25519_dalek::{Signer, SigningKey};

/// Parses an Ed25519 PKCS#8 PEM private key into a signing key.
pub fn load_key_from_pem(pem: &str) -> Result<SigningKey, String> {
    let b64: String = pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<_>>()
        .concat();
    let der = B64
        .decode(b64.trim())
        .map_err(|e| format!("bad PEM: {e}"))?;
    if der.len() < 32 {
        return Err("key too short".into());
    }
    // PKCS#8 Ed25519 is 48 bytes; the seed is the last 32.
    let seed: [u8; 32] = der[der.len() - 32..].try_into().unwrap();
    Ok(SigningKey::from_bytes(&seed))
}

/// Signs the Logon payload: MsgType|SenderCompID|TargetCompID|MsgSeqNum|SendingTime
/// (SOH-joined) and returns it base64-encoded for tag 96.
pub fn sign_logon(key: &SigningKey, sender: &str, target: &str, seq: u64, time: &str) -> String {
    let payload = format!("A\x01{sender}\x01{target}\x01{seq}\x01{time}");
    B64.encode(key.sign(payload.as_bytes()).to_bytes())
}
