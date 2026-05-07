use sdk_contracts::{
    AddressData, AssetBalanceData, BtcBalance, BtcBalanceData, ChannelData, ChannelStatus,
    DecodeLnInvoiceData, NetworkInfoData, NodeInfoData, PeerData,
};

fn rln_types_json(path: &str) -> String {
    match path {
        "network_info.json" => include_str!("rln_types_json/network_info.json"),
        "node_info.json" => include_str!("rln_types_json/node_info.json"),
        "list_channels.json" => include_str!("rln_types_json/list_channels.json"),
        "list_peers.json" => include_str!("rln_types_json/list_peers.json"),
        "decode_ln_invoice.json" => include_str!("rln_types_json/decode_ln_invoice.json"),
        "address.json" => include_str!("rln_types_json/address.json"),
        "btc_balance.json" => include_str!("rln_types_json/btc_balance.json"),
        "asset_balance.json" => include_str!("rln_types_json/asset_balance.json"),
        "invoice_status.json" => include_str!("rln_types_json/invoice_status.json"),
        "estimate_fee.json" => include_str!("rln_types_json/estimate_fee.json"),
        "sign_message.json" => include_str!("rln_types_json/sign_message.json"),
        "asset_media.json" => include_str!("rln_types_json/asset_media.json"),
        "channel_id.json" => include_str!("rln_types_json/channel_id.json"),
        "ln_invoice.json" => include_str!("rln_types_json/ln_invoice.json"),
        _ => panic!("unknown rln_types_json fixture: {path}"),
    }
    .trim()
    .to_string()
}

#[test]
fn rln_types_network_info_json() {
    let v = NetworkInfoData {
        network: "testnet4".to_string(),
        height: 12345,
    };
    assert_eq!(
        serde_json::to_string(&v).unwrap(),
        rln_types_json("network_info.json")
    );
}

#[test]
fn rln_types_node_info_json() {
    let v = NodeInfoData {
        pubkey: "02aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        num_channels: 2,
        num_usable_channels: 1,
        local_balance_sat: 1000,
        eventual_close_fees_sat: 2,
        pending_outbound_payments_sat: 3,
        num_peers: 4,
        account_xpub_vanilla: "xpub-vanilla".to_string(),
        account_xpub_colored: "xpub-colored".to_string(),
        max_media_upload_size_mb: 25,
        rgb_htlc_min_msat: 5_000_000,
        rgb_channel_capacity_min_sat: 1_000_000,
        channel_capacity_min_sat: 10_000,
        channel_capacity_max_sat: 16_777_215,
        channel_asset_min_amount: 1,
        channel_asset_max_amount: u64::MAX,
        network_nodes: 10,
        network_channels: 11,
    };
    assert_eq!(
        serde_json::to_string(&v).unwrap(),
        rln_types_json("node_info.json")
    );
}

#[test]
fn rln_types_list_channels_json() {
    let v = vec![ChannelData {
        channel_id: "deadbeef".to_string(),
        funding_txid: None,
        peer_pubkey: "02bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            .to_string(),
        peer_alias: Some("peer-a".to_string()),
        short_channel_id: Some(42),
        status: ChannelStatus::Opened,
        ready: true,
        capacity_sat: 100_000,
        local_balance_sat: 50_000,
        outbound_balance_msat: 60_000,
        inbound_balance_msat: 40_000,
        next_outbound_htlc_limit_msat: 70_000,
        next_outbound_htlc_minimum_msat: 1000,
        is_usable: true,
        public: false,
        asset_id: None,
        asset_local_amount: None,
        asset_remote_amount: None,
        virtual_open_mode: None,
    }];
    assert_eq!(
        serde_json::to_string(&v).unwrap(),
        rln_types_json("list_channels.json")
    );
}

#[test]
fn rln_types_list_peers_json() {
    let v = vec![PeerData {
        pubkey: "03cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_string(),
    }];
    assert_eq!(
        serde_json::to_string(&v).unwrap(),
        rln_types_json("list_peers.json")
    );
}

#[test]
fn rln_types_decode_ln_invoice_json() {
    let v = DecodeLnInvoiceData {
        amt_msat: Some(1234),
        expiry_sec: 3600,
        timestamp: 1_700_000_000,
        asset_id: None,
        asset_amount: None,
        payment_hash: "001122".to_string(),
        payment_secret: "334455".to_string(),
        payee_pubkey: None,
        network: "testnet4".to_string(),
    };
    assert_eq!(
        serde_json::to_string(&v).unwrap(),
        rln_types_json("decode_ln_invoice.json")
    );
}

#[test]
fn rln_types_address_json() {
    let v = AddressData {
        address: "bcrt1qexampleaddress".to_string(),
    };
    assert_eq!(
        serde_json::to_string(&v).unwrap(),
        rln_types_json("address.json")
    );
}

#[test]
fn rln_types_btc_balance_json() {
    let v = BtcBalanceData {
        vanilla: BtcBalance {
            settled: 1,
            future: 2,
            spendable: 3,
        },
        colored: BtcBalance {
            settled: 4,
            future: 5,
            spendable: 6,
        },
    };
    assert_eq!(
        serde_json::to_string(&v).unwrap(),
        rln_types_json("btc_balance.json")
    );
}

#[test]
fn rln_types_asset_balance_json() {
    let v = AssetBalanceData {
        settled: 10,
        future: 11,
        spendable: 12,
        offchain_outbound: 13,
        offchain_inbound: 14,
    };
    assert_eq!(
        serde_json::to_string(&v).unwrap(),
        rln_types_json("asset_balance.json")
    );
}

#[test]
fn rln_types_invoice_status_json() {
    let v = sdk_contracts::InvoiceStatusData {
        status: sdk_contracts::InvoiceStatus::Pending,
    };
    assert_eq!(
        serde_json::to_string(&v).unwrap(),
        rln_types_json("invoice_status.json")
    );
}

#[test]
fn rln_types_estimate_fee_json() {
    let v = sdk_contracts::EstimateFeeData { fee_rate: 1.5 };
    assert_eq!(
        serde_json::to_string(&v).unwrap(),
        rln_types_json("estimate_fee.json")
    );
}

#[test]
fn rln_types_sign_message_json() {
    let v = sdk_contracts::SignMessageData {
        signed_message: "30440220deadbeef".to_string(),
    };
    assert_eq!(
        serde_json::to_string(&v).unwrap(),
        rln_types_json("sign_message.json")
    );
}

#[test]
fn rln_types_asset_media_json() {
    let v = sdk_contracts::AssetMediaData {
        bytes_hex: "00ff".to_string(),
    };
    assert_eq!(
        serde_json::to_string(&v).unwrap(),
        rln_types_json("asset_media.json")
    );
}

#[test]
fn rln_types_channel_id_json() {
    let v = sdk_contracts::ChannelIdData {
        channel_id: "abc123".to_string(),
    };
    assert_eq!(
        serde_json::to_string(&v).unwrap(),
        rln_types_json("channel_id.json")
    );
}

#[test]
fn rln_types_ln_invoice_json() {
    let v = sdk_contracts::LnInvoiceData {
        invoice: "lnbc1example".to_string(),
    };
    assert_eq!(
        serde_json::to_string(&v).unwrap(),
        rln_types_json("ln_invoice.json")
    );
}
