use rgb_lib_wasm::wallet::{DatabaseType, WalletData};
use rgb_lib_wasm::{AssetSchema, BitcoinNetwork};

pub(crate) fn test_wallet_data_json() -> String {
    let keys = rgb_lib_wasm::generate_keys(BitcoinNetwork::Regtest);
    let wd = WalletData {
        data_dir: "/tmp/rln_wasm_sdk_tests".to_string(),
        bitcoin_network: BitcoinNetwork::Regtest,
        database_type: DatabaseType::Sqlite,
        max_allocations_per_utxo: 5,
        account_xpub_vanilla: keys.account_xpub_vanilla,
        account_xpub_colored: keys.account_xpub_colored,
        mnemonic: Some(keys.mnemonic),
        master_fingerprint: keys.master_fingerprint,
        vanilla_keychain: None,
        supported_schemas: vec![AssetSchema::Nia],
    };
    serde_json::to_string(&wd).expect("wallet data json")
}
