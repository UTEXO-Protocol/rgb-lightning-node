//! Protocol-1 batch acceptance: mixed assignments and witness receive share one native fee/tx.
use super::*;

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
async fn authenticated_invoice_batch_sends_nia_witness_and_nft_in_one_transaction() {
    initialize();
    let (sender, _) = start_node(
        "tmp/authenticated_invoice_batch/sender",
        NODE1_PEER_PORT,
        false,
    )
    .await;
    let (receiver, _) = start_node(
        "tmp/authenticated_invoice_batch/receiver",
        NODE2_PEER_PORT,
        false,
    )
    .await;
    fund_and_create_utxos(sender, None).await;
    fund_and_create_utxos(receiver, None).await;
    let nia = issue_asset_nia(sender).await.asset_id;
    let nft = issue_asset_uda(sender, None).await.asset_id;
    let nia_invoice = rgb_invoice_with_assignment(
        receiver,
        Some(nia.clone()),
        Some(Assignment::Fungible(100)),
        true,
    )
    .await;
    let nft_invoice = rgb_invoice_with_assignment(
        receiver,
        Some(nft.clone()),
        Some(Assignment::NonFungible),
        false,
    )
    .await;
    let before = list_transactions(sender).await;
    let invalid = serde_json::json!({
        "donation": true, "fee_rate": FEE_RATE, "min_confirmations": 1,
        "recipient_groups": [
            {"asset_id": nia, "recipients": [{"invoice": nia_invoice.invoice, "assignment": {"type":"Fungible","value":100}, "witness_data":{"amount_sat":1200,"blinding":null}}]},
            {"asset_id": nia, "recipients": [{"invoice": nft_invoice.invoice, "assignment":{"type":"NonFungible"}, "witness_data":null}]}
        ]
    });
    let res = reqwest::Client::new()
        .post(format!("http://{sender}/sendrgbinvoices"))
        .json(&invalid)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        list_transactions(sender).await.len(),
        before.len(),
        "invalid batch must not reserve or broadcast"
    );
    let txid = send_assets(
        sender,
        HashMap::from([
            (
                nia.clone(),
                vec![Recipient {
                    recipient_id: nia_invoice.recipient_id,
                    witness_data: Some(WitnessData {
                        amount_sat: 1200,
                        blinding: None,
                    }),
                    assignment: Assignment::Fungible(100),
                    transport_endpoints: vec![PROXY_ENDPOINT_LOCAL.into()],
                }],
            ),
            (
                nft.clone(),
                vec![Recipient {
                    recipient_id: nft_invoice.recipient_id,
                    witness_data: None,
                    assignment: Assignment::NonFungible,
                    transport_endpoints: vec![PROXY_ENDPOINT_LOCAL.into()],
                }],
            ),
        ]),
        true,
    )
    .await;
    mine(false);
    wait_for_balance(receiver, &nia, 100).await;
    wait_for_balance(receiver, &nft, 1).await;
    wait_for_balance(sender, &nia, 900).await;
    wait_for_balance(sender, &nft, 0).await;
    let transactions = list_transactions(sender).await;
    assert_eq!(
        transactions.len(),
        before.len() + 1,
        "the batch must pay only one Bitcoin transaction fee"
    );
    let transaction = transactions
        .iter()
        .find(|t| t.txid == txid)
        .expect("one recorded native batch transaction");
    assert!(transaction.confirmation_time.is_some());
    assert!(transaction.fee > 0);
    for asset in [&nia, &nft] {
        assert!(list_transfers(sender, asset)
            .await
            .iter()
            .any(
                |t| t.txid.as_deref() == Some(txid.as_str()) && t.status == TransferStatus::Settled
            ));
        assert!(list_transfers(receiver, asset)
            .await
            .iter()
            .any(
                |t| t.txid.as_deref() == Some(txid.as_str()) && t.status == TransferStatus::Settled
            ));
    }
    shutdown(&[sender, receiver]).await;
}
