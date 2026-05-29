use crate::routes::UnlockRequest;

#[test]
fn deserialize_without_bitcoind_fields() {
    let json = serde_json::json!({
        "password": "p",
        "indexer_url": "https://blockstream.info/testnet/api",
        "proxy_endpoint": "rpc://127.0.0.1:3000/json-rpc",
        "announce_addresses": [],
    });
    let req: UnlockRequest = serde_json::from_value(json).unwrap();
    assert!(req.bitcoind_rpc_username.is_none());
    assert!(req.bitcoind_rpc_password.is_none());
    assert!(req.bitcoind_rpc_host.is_none());
    assert!(req.bitcoind_rpc_port.is_none());
    assert_eq!(
        req.indexer_url.as_deref(),
        Some("https://blockstream.info/testnet/api")
    );
}

#[test]
fn deserialize_with_bitcoind_fields() {
    let json = serde_json::json!({
        "password": "p",
        "bitcoind_rpc_username": "user",
        "bitcoind_rpc_password": "password",
        "bitcoind_rpc_host": "localhost",
        "bitcoind_rpc_port": 18443,
        "announce_addresses": [],
    });
    let req: UnlockRequest = serde_json::from_value(json).unwrap();
    assert_eq!(req.bitcoind_rpc_username.as_deref(), Some("user"));
    assert_eq!(req.bitcoind_rpc_port, Some(18443));
}
