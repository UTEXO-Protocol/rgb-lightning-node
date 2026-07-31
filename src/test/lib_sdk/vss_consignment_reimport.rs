//! Issue #111: a node restored from VSS without an RGB wallet backup must
//! still recover the funds of force-closed channels — vanilla and colored —
//! by re-importing the channel consignment kept in the replicated KV data.
//! Requires the regtest stack (`./regtest.sh start`) and the VSS server
//! (`docker compose --profile vss up -d`).

use crate::helpers::*;
use crate::vss_manager_lag::{vss_server_available, ManagerFilterProxy};
use serial_test::serial;
use std::{fs, time::Duration};

const NODE_A_PORT_OFFSET: u16 = 150;
const NODE_B_PORT_OFFSET: u16 = 150;
const PASSWORD_A: &str = "nodeApass";
const PASSWORD_B: &str = "nodeBpass";
const CHANNEL_ASSET_AMOUNT: u64 = 600;
const ASSET_SEND_A_TO_B: u64 = 150;
const ASSET_SEND_B_TO_A: u64 = 50;
// The recovery must reflect the latest commitment, not the funding split.
const NODE_A_FINAL_ASSET_AMOUNT: u64 = CHANNEL_ASSET_AMOUNT - ASSET_SEND_A_TO_B + ASSET_SEND_B_TO_A;
const NODE_B_FINAL_ASSET_AMOUNT: u64 = ASSET_SEND_A_TO_B - ASSET_SEND_B_TO_A;

fn spendable_sats(node: &SdkNode) -> u64 {
    let balance = node.btc_balance(false).expect("btc_balance");
    balance.vanilla.spendable + balance.colored.spendable
}

#[test]
#[serial]
fn restored_node_recovers_force_closed_channel_funds() {
    ensure_regtest_available();
    if !vss_server_available() {
        eprintln!("SKIP: VSS server not available");
        return;
    }

    let test_dir = test_dir("vss_consignment_reimport");
    if test_dir.exists() {
        fs::remove_dir_all(&test_dir).expect("clean previous test dir");
    }
    fs::create_dir_all(&test_dir).expect("create test dir");
    let node_a_dir = test_dir.join("node_a");
    let node_b_dir = test_dir.join("node_b");

    let proxy = ManagerFilterProxy::start();

    // Phase 1: build the incomplete backup through the product path — the
    // node replicates normally except that no RGB backup ever reaches VSS.
    let node_a = make_node_with_vss(
        &node_a_dir,
        NODE_A_DAEMON_PORT + NODE_A_PORT_OFFSET,
        NODE_A_PEER_PORT + NODE_A_PORT_OFFSET,
        &proxy.url(),
    );
    let node_b = make_node(
        &node_b_dir,
        NODE_B_DAEMON_PORT + NODE_B_PORT_OFFSET,
        NODE_B_PEER_PORT + NODE_B_PORT_OFFSET,
    );

    let mnemonic_a = node_a
        .init(PASSWORD_A.to_string(), None)
        .expect("node A init");
    node_b
        .init(PASSWORD_B.to_string(), None)
        .expect("node B init");
    node_a
        .unlock(unlock_request(PASSWORD_A))
        .expect("node A initial unlock");
    node_b
        .unlock(unlock_request(PASSWORD_B))
        .expect("node B initial unlock");

    proxy.block_rgb_backup_writes();

    fund_and_create_utxos(&node_a, "node A");
    fund_and_create_utxos(&node_b, "node B");

    let asset = node_a
        .issueassetnia(SdkIssueAssetNiaRequest {
            amounts: vec![1_000],
            ticker: "USDT".to_string(),
            name: "Tether".to_string(),
            precision: 0,
        })
        .expect("node A issueassetnia");
    let asset_id = asset.asset_id;

    let node_a_pubkey = node_a.node_info().expect("node A node_info").pubkey;
    let node_b_pubkey = node_b.node_info().expect("node B node_info").pubkey;
    let peer_uri = format!(
        "{node_b_pubkey}@127.0.0.1:{}",
        NODE_B_PEER_PORT + NODE_B_PORT_OFFSET
    );
    node_a
        .connectpeer(peer_uri.clone())
        .expect("node A connectpeer");

    let open_channel = node_a
        .openchannel(SdkOpenChannelRequest {
            peer_pubkey_and_opt_addr: peer_uri.clone(),
            capacity_sat: OPEN_CHANNEL_CAPACITY_SAT,
            // Push some msat so node B can pay the return keysend over the
            // channel reserve.
            push_msat: PAYMENT_MSAT,
            public: true,
            with_anchors: true,
            fee_base_msat: None,
            fee_proportional_millionths: None,
            temporary_channel_id: None,
            asset_id: Some(asset_id),
            asset_amount: Some(CHANNEL_ASSET_AMOUNT),
            push_asset_amount: None,
            virtual_open_mode: None,
        })
        .expect("node A openchannel colored");
    wait_for_channel_funding_tx(&node_a, &node_b, &asset_id, Duration::from_secs(120));
    mine(OPEN_CHANNEL_CONFIRM_BLOCKS);
    wait_for_usable_channel(&node_a, &node_b, &asset_id, Duration::from_secs(300));
    let _colored_channel_id = node_a
        .get_channel_id(open_channel.temporary_channel_id)
        .expect("node A get_channel_id");

    // A second, vanilla channel: its force-close sweep must also recover.
    node_a
        .openchannel(SdkOpenChannelRequest {
            peer_pubkey_and_opt_addr: peer_uri.clone(),
            capacity_sat: OPEN_CHANNEL_CAPACITY_SAT,
            push_msat: 0,
            public: true,
            with_anchors: true,
            fee_base_msat: None,
            fee_proportional_millionths: None,
            temporary_channel_id: None,
            asset_id: None,
            asset_amount: None,
            push_asset_amount: None,
            virtual_open_mode: None,
        })
        .expect("node A openchannel vanilla");
    mine(OPEN_CHANNEL_CONFIRM_BLOCKS);
    wait_for_usable_channel_counts(&[(&node_a, 2), (&node_b, 2)], Duration::from_secs(300));

    // Several settled payments so the stored fascia is overwritten across
    // commitment updates and the restore recovers the latest balance split.
    keysend(
        &node_a,
        node_b_pubkey,
        None,
        Some(&asset_id),
        Some(ASSET_SEND_A_TO_B),
    );
    keysend(
        &node_b,
        node_a_pubkey,
        None,
        Some(&asset_id),
        Some(ASSET_SEND_B_TO_A),
    );
    std::thread::sleep(Duration::from_secs(5));

    // Phase 2: graceful shutdown, device wiped, restore from VSS + seed.
    node_a.shutdown();
    drop(node_a);
    proxy.allow_all();
    fs::remove_dir_all(&node_a_dir).expect("wipe node A storage");

    let node_a = make_node_with_vss(
        &node_a_dir,
        NODE_A_DAEMON_PORT + NODE_A_PORT_OFFSET,
        NODE_A_PEER_PORT + NODE_A_PORT_OFFSET,
        &proxy.url(),
    );
    let mnemonic_returned = node_a
        .init(PASSWORD_A.to_string(), Some(mnemonic_a.clone()))
        .expect("node A re-init");
    assert_eq!(mnemonic_returned, mnemonic_a);
    node_a
        .vss_clear_fence(SdkVssClearFenceRequest {
            password: PASSWORD_A.to_string(),
        })
        .expect("node A vss_clear_fence");
    node_a
        .unlock(unlock_request(PASSWORD_A))
        .expect("node A unlock after restore");

    // Phase 3: right after unlock the wallet must know the channel asset
    // again, re-imported from the consignment in the replicated KV data.
    let assets = node_a
        .list_assets(vec![])
        .expect("node A list_assets")
        .nia
        .unwrap_or_default();
    assert!(
        assets.iter().any(|a| a.asset_id == asset_id),
        "restored node must re-import the channel asset from the stored consignment"
    );

    node_a
        .connectpeer(peer_uri)
        .expect("node A connectpeer after restore");
    wait_for_usable_channels(&node_a, 2, Duration::from_secs(120));
    let btc_at_restore = spendable_sats(&node_a);

    // Phase 4: force-close both channels; the sweeps must return the BTC of
    // both to_self outputs and make the channel's asset amount spendable.
    let channels = node_a.list_channels().expect("node A list_channels");
    assert_eq!(channels.len(), 2);
    for channel in channels {
        close_channel_with_force(&node_a, channel.channel_id, node_b_pubkey, true);
    }

    let mut btc_recovered = false;
    let mut assets_recovered = false;
    for _ in 0..40 {
        mine(10);
        std::thread::sleep(Duration::from_secs(3));
        let _ = node_a.refreshtransfers(SdkRefreshTransfersRequest { skip_sync: false });
        // > 120k sat proves both ~97k to_self outputs (vanilla and colored)
        // were swept; either alone cannot reach it.
        if !btc_recovered && spendable_sats(&node_a) > btc_at_restore + 120_000 {
            btc_recovered = true;
        }
        if !assets_recovered
            && asset_balance_spendable(&node_a, &asset_id) >= NODE_A_FINAL_ASSET_AMOUNT
        {
            assets_recovered = true;
        }
        if btc_recovered && assets_recovered {
            break;
        }
    }
    assert!(
        btc_recovered,
        "the force-closed channels' BTC (vanilla and colored) must return to the spendable balance"
    );
    assert!(
        assets_recovered,
        "the channel's asset amount must become spendable again"
    );

    // The intact counterparty must also claim its latest-state amount.
    wait_for_balance(
        &node_b,
        &asset_id,
        NODE_B_FINAL_ASSET_AMOUNT,
        Duration::from_secs(120),
    );

    node_a.shutdown();
    node_b.shutdown();
}
