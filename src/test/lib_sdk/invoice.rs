use crate::helpers::*;
use bitcoin::hashes::sha256::Hash as Sha256;
use bitcoin::hashes::Hash;
use lightning_invoice::Bolt11InvoiceDescriptionRef;
use serial_test::serial;
use std::fs;

#[test]
#[serial]
fn description_hash_survives_sdk_path() {
    ensure_regtest_available();

    let test_dir = test_dir("sdk_invoice_description_hash");
    if test_dir.exists() {
        fs::remove_dir_all(&test_dir).expect("remove previous lib_sdk invoice test dir");
    }
    fs::create_dir_all(&test_dir).expect("create lib_sdk invoice test dir");
    let node_dir = test_dir.join("node");

    let node = make_node(&node_dir, NODE_A_DAEMON_PORT + 90, NODE_A_PEER_PORT + 90);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        node.init("nodeApass".to_string(), None).expect("node init");
        node.unlock(unlock_request("nodeApass"))
            .expect("node unlock");

        let description_hash = lightning_invoice::Sha256(Sha256::hash(b"out-of-band description"));
        let invoice = node
            .ln_invoice(LnInvoiceRequest {
                amt_msat: None,
                expiry_sec: 900,
                asset_id: None,
                asset_amount: None,
                payment_hash: None,
                description: None,
                description_hash: Some(description_hash.0.to_string()),
                min_final_cltv_expiry_delta: None,
            })
            .expect("node ln_invoice with description_hash")
            .invoice;

        assert!(matches!(
            invoice.description(),
            Bolt11InvoiceDescriptionRef::Hash(hash) if *hash == description_hash
        ));
    }));

    node.shutdown();
    result.unwrap();
}

#[test]
#[serial]
fn description_survives_sdk_path() {
    ensure_regtest_available();

    let test_dir = test_dir("sdk_invoice_description");
    if test_dir.exists() {
        fs::remove_dir_all(&test_dir).expect("remove previous lib_sdk invoice test dir");
    }
    fs::create_dir_all(&test_dir).expect("create lib_sdk invoice test dir");
    let node_dir = test_dir.join("node");

    let node = make_node(&node_dir, NODE_A_DAEMON_PORT + 100, NODE_A_PEER_PORT + 100);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        node.init("nodeApass".to_string(), None).expect("node init");
        node.unlock(unlock_request("nodeApass"))
            .expect("node unlock");

        let description = "1 cup of coffee";
        let invoice = node
            .ln_invoice(LnInvoiceRequest {
                amt_msat: None,
                expiry_sec: 900,
                asset_id: None,
                asset_amount: None,
                payment_hash: None,
                description: Some(description.to_string()),
                description_hash: None,
                min_final_cltv_expiry_delta: None,
            })
            .expect("node ln_invoice with description")
            .invoice;

        assert!(matches!(
            invoice.description(),
            Bolt11InvoiceDescriptionRef::Direct(d) if d.to_string() == description
        ));

        let too_long = node.ln_invoice(LnInvoiceRequest {
            amt_msat: None,
            expiry_sec: 900,
            asset_id: None,
            asset_amount: None,
            payment_hash: None,
            description: Some("a".repeat(640)),
            description_hash: None,
            min_final_cltv_expiry_delta: None,
        });
        assert!(matches!(
            too_long,
            Err(rgb_lightning_node::RlnError::InvalidRequest(_))
        ));

        let both = node.ln_invoice(LnInvoiceRequest {
            amt_msat: None,
            expiry_sec: 900,
            asset_id: None,
            asset_amount: None,
            payment_hash: None,
            description: Some(description.to_string()),
            description_hash: Some(
                "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff".to_string(),
            ),
            min_final_cltv_expiry_delta: None,
        });
        assert!(matches!(
            both,
            Err(rgb_lightning_node::RlnError::InvalidRequest(_))
        ));
    }));

    node.shutdown();
    result.unwrap();
}
