use crate::core_types::LdkChainSync;
use crate::routes::UnlockRequest;

#[cfg(feature = "transaction-sync")]
#[test]
fn deserialize_transaction_sync_mode() {
    let json = serde_json::json!({
        "password": "p",
        "ldk_chain_sync": {
            "mode": "TransactionSync",
            "config": { "indexer_url": "https://blockstream.info/testnet/api" },
        },
        "indexer_url": "https://blockstream.info/testnet/api",
        "proxy_endpoint": "rpc://127.0.0.1:3000/json-rpc",
        "announce_addresses": [],
    });
    let req: UnlockRequest = serde_json::from_value(json).unwrap();
    assert!(matches!(
        req.ldk_chain_sync,
        LdkChainSync::TransactionSync { ref indexer_url }
            if indexer_url == "https://blockstream.info/testnet/api"
    ));
    assert_eq!(
        req.indexer_url.as_deref(),
        Some("https://blockstream.info/testnet/api")
    );
}

#[cfg(feature = "block-sync")]
#[test]
fn deserialize_block_sync_mode() {
    let json = serde_json::json!({
        "password": "p",
        "ldk_chain_sync": {
            "mode": "BlockSync",
            "config": {
                "bitcoind_rpc_username": "user",
                "bitcoind_rpc_password": "password",
                "bitcoind_rpc_host": "localhost",
                "bitcoind_rpc_port": 18443,
            },
        },
        "indexer_url": "ssl://electrum.iriswallet.com:50013",
        "announce_addresses": [],
    });
    let req: UnlockRequest = serde_json::from_value(json).unwrap();
    assert!(matches!(
        req.ldk_chain_sync,
        LdkChainSync::BlockSync {
            ref bitcoind_rpc_username,
            bitcoind_rpc_port,
            ..
        } if bitcoind_rpc_username == "user" && bitcoind_rpc_port == 18443
    ));
}

// bitcoind for LDK chain data and esplora for the RGB wallet is expressible: the sync mode and
// the wallet indexer are independent, so neither can make the other ambiguous
#[cfg(feature = "block-sync")]
#[test]
fn block_sync_with_esplora_indexer_is_accepted() {
    let json = serde_json::json!({
        "password": "p",
        "ldk_chain_sync": {
            "mode": "BlockSync",
            "config": {
                "bitcoind_rpc_username": "user",
                "bitcoind_rpc_password": "password",
                "bitcoind_rpc_host": "localhost",
                "bitcoind_rpc_port": 18443,
            },
        },
        "indexer_url": "https://blockstream.info/testnet/api",
        "announce_addresses": [],
    });
    let req: UnlockRequest = serde_json::from_value(json).unwrap();
    assert!(matches!(req.ldk_chain_sync, LdkChainSync::BlockSync { .. }));
    assert_eq!(
        req.indexer_url.as_deref(),
        Some("https://blockstream.info/testnet/api")
    );
}

// the sync mode is mandatory: there is no implicit selection left to get wrong
#[cfg(feature = "block-sync")]
#[test]
fn deserialize_without_chain_sync_errors() {
    let json = serde_json::json!({
        "password": "p",
        "indexer_url": "ssl://electrum.iriswallet.com:50013",
        "announce_addresses": [],
    });
    assert!(serde_json::from_value::<UnlockRequest>(json).is_err());
}

// a partially specified block-sync config is rejected by serde, not by a runtime check
#[cfg(feature = "block-sync")]
#[test]
fn deserialize_partial_block_sync_errors() {
    let json = serde_json::json!({
        "password": "p",
        "ldk_chain_sync": {
            "mode": "BlockSync",
            "config": {
                "bitcoind_rpc_username": "user",
                "bitcoind_rpc_password": "password",
                "bitcoind_rpc_host": "localhost",
            },
        },
        "indexer_url": "ssl://electrum.iriswallet.com:50013",
        "announce_addresses": [],
    });
    assert!(serde_json::from_value::<UnlockRequest>(json).is_err());
}

// the indexer_url may be omitted from the request; it then comes from the `[chain]` config section
#[cfg(feature = "block-sync")]
#[test]
fn deserialize_without_indexer_url() {
    let json = serde_json::json!({
        "password": "p",
        "ldk_chain_sync": {
            "mode": "BlockSync",
            "config": {
                "bitcoind_rpc_username": "user",
                "bitcoind_rpc_password": "password",
                "bitcoind_rpc_host": "localhost",
                "bitcoind_rpc_port": 18443,
            },
        },
        "indexer_url": null,
        "announce_addresses": [],
    });
    let req: UnlockRequest = serde_json::from_value(json).unwrap();
    assert!(req.indexer_url.is_none());
}
