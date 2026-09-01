use super::super::*;
use super::*;

const TEST_DIR_BASE: &str = "tmp/interoperability_lnd/";

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
// run with `cargo test -- --ignored interoperability`, needs `lnd`/`lncli` on PATH
#[ignore]
async fn stock_lnd_opens_pays_both_ways_and_closes() {
    if interop_backend().as_str() != "lnd" {
        return;
    }
    initialize();

    let test_dir_node1 = format!("{TEST_DIR_BASE}node1");
    let test_dir_stock = format!("{TEST_DIR_BASE}stock");
    let (node1_addr, _) = start_node(&test_dir_node1, NODE1_PEER_PORT, false).await;
    fund_and_create_utxos(node1_addr, None).await;

    let mut stock = StockLndNode::start(&test_dir_stock, NODE2_PEER_PORT);
    let stock_address = stock.onchain_address();
    fund_wallet(stock_address.to_string(), 100_000_000);
    wait_for_stock_funds(&mut stock).await;

    scenario_stock_opens_pays_both_ways_and_closes(node1_addr, &mut stock).await;
    stock.stop();
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
// run with `cargo test -- --ignored interoperability`, needs `lnd`/`lncli` on PATH
#[ignore]
async fn rln_opens_to_stock_lnd_pays_both_ways_and_closes() {
    if interop_backend().as_str() != "lnd" {
        return;
    }
    initialize();

    let test_dir_node1 = format!("{TEST_DIR_BASE}node1");
    let test_dir_stock = format!("{TEST_DIR_BASE}stock");
    let (node1_addr, _) = start_node(&test_dir_node1, NODE1_PEER_PORT, false).await;
    fund_and_create_utxos(node1_addr, None).await;

    let mut stock = StockLndNode::start(&test_dir_stock, NODE2_PEER_PORT);
    // MUST fund the stock node so it has reserves for anchor channels!
    let stock_address = stock.onchain_address();
    fund_wallet(stock_address.to_string(), 100_000_000);
    wait_for_stock_funds(&mut stock).await;

    scenario_rln_opens_pays_both_ways_and_closes(node1_addr, &mut stock).await;
    stock.stop();
}

#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[traced_test]
#[ignore]
async fn rgb_channel_to_stock_lnd_fails() {
    if interop_backend().as_str() != "lnd" {
        return;
    }
    initialize();

    let test_dir_node1 = format!("{TEST_DIR_BASE}node1");
    let test_dir_stock = format!("{TEST_DIR_BASE}stock");
    let (node1_addr, _) = start_node(&test_dir_node1, NODE1_PEER_PORT, false).await;
    fund_and_create_utxos(node1_addr, None).await;

    let mut stock = StockLndNode::start(&test_dir_stock, NODE2_PEER_PORT);
    scenario_rgb_channel_to_stock_fails(node1_addr, &mut stock).await;
    stock.stop();
}