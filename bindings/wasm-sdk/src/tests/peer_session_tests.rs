use secp256k1::PublicKey as SecpPublicKey;
use std::str::FromStr;

#[test]
fn peer_pubkey_validation_contract() {
    let valid = "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    assert!(SecpPublicKey::from_str(&valid).is_ok());
    assert!(SecpPublicKey::from_str("not-a-pubkey").is_err());
}
