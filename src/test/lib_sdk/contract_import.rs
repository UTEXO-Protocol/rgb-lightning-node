use crate::helpers::*;
use base64::{engine::general_purpose, Engine as _};
use rgb_lightning_node::{ImportRgbContractRequest, RlnError};
use serial_test::serial;
use std::fs;

fn export_contract_base64(node: &SdkNode, asset_id: &ContractId) -> String {
    let bytes = rgb_lightning_node::test_utils::export_rgb_contract_bytes_for_tests(
        node,
        &asset_id.to_string(),
    );
    general_purpose::STANDARD.encode(bytes)
}

#[test]
#[serial]
fn contract_import_roundtrips_through_sdk() {
    ensure_regtest_available();

    let test_dir = test_dir("sdk_contract_import");
    if test_dir.exists() {
        fs::remove_dir_all(&test_dir).expect("remove previous SDK contract import test dir");
    }
    fs::create_dir_all(&test_dir).expect("create SDK contract import test dir");
    let issuer = make_node(
        &test_dir.join("issuer"),
        NODE_A_DAEMON_PORT + 120,
        NODE_A_PEER_PORT + 120,
    );
    let recipient = make_node(
        &test_dir.join("recipient"),
        NODE_B_DAEMON_PORT + 120,
        NODE_B_PEER_PORT + 120,
    );

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        issuer
            .init("issuer-pass".to_string(), None)
            .expect("issuer init");
        recipient
            .init("recipient-pass".to_string(), None)
            .expect("recipient init");
        issuer
            .unlock(unlock_request("issuer-pass"))
            .expect("issuer unlock");
        recipient
            .unlock(unlock_request("recipient-pass"))
            .expect("recipient unlock");

        fund_and_create_utxos(&issuer, "issuer");
        let issued = issuer
            .issueassetnia(SdkIssueAssetNiaRequest {
                amounts: vec![10_000],
                ticker: "USDT".to_string(),
                name: "Test USD".to_string(),
                precision: 2,
            })
            .expect("issue RGB contract");
        let contract_base64 = export_contract_base64(&issuer, &issued.asset_id);

        let imported = recipient
            .importrgbcontract(ImportRgbContractRequest {
                contract_base64: contract_base64.clone(),
                expected_asset_id: issued.asset_id.clone(),
            })
            .expect("import RGB contract through SDK");
        assert_eq!(imported.asset_id, issued.asset_id);
        assert!(!imported.already_imported);
        assert_eq!(imported.metadata.asset_schema, "Nia");
        assert_eq!(imported.metadata.name, "Test USD");
        assert_eq!(imported.metadata.ticker.as_deref(), Some("USDT"));
        assert_eq!(imported.metadata.precision, 2);

        let balance = recipient
            .asset_balance(issued.asset_id.clone())
            .expect("imported contract balance");
        assert_eq!(balance.settled, 0);
        assert_eq!(balance.future, 0);
        assert_eq!(balance.spendable, 0);
        assert_eq!(balance.offchain_outbound, 0);
        assert_eq!(balance.offchain_inbound, 0);

        let repeated = recipient
            .importrgbcontract(ImportRgbContractRequest {
                contract_base64: contract_base64.clone(),
                expected_asset_id: issued.asset_id.clone(),
            })
            .expect("repeat RGB contract import");
        assert!(repeated.already_imported);
        assert_eq!(repeated.asset_id, issued.asset_id);

        let other_asset = issuer
            .issueassetnia(SdkIssueAssetNiaRequest {
                amounts: vec![1],
                ticker: "OTHER".to_string(),
                name: "Other asset".to_string(),
                precision: 0,
            })
            .expect("issue mismatched RGB contract");
        let other_contract_base64 = export_contract_base64(&issuer, &other_asset.asset_id);
        let mismatch = recipient.importrgbcontract(ImportRgbContractRequest {
            contract_base64: other_contract_base64,
            expected_asset_id: issued.asset_id,
        });
        assert!(matches!(mismatch, Err(RlnError::InvalidRequest(_))));
        assert!(matches!(
            recipient.asset_metadata(other_asset.asset_id),
            Err(RlnError::NotFound(_))
        ));
    }));

    issuer.shutdown();
    recipient.shutdown();
    result.unwrap();
}
