use super::*;

pub(crate) async fn address(
    State(state): State<Arc<AppState>>,
) -> Result<Json<AddressResponse>, APIError> {
    let data = sdk::address(state.clone()).await?;
    Ok(Json(AddressResponse {
        address: data.address,
    }))
}

pub(crate) async fn btc_balance(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<BtcBalanceRequest>, APIError>,
) -> Result<Json<BtcBalanceResponse>, APIError> {
    let data = sdk::btc_balance(state.clone(), payload.skip_sync).await?;
    Ok(Json(BtcBalanceResponse {
        vanilla: BtcBalance {
            settled: data.vanilla.settled,
            future: data.vanilla.future,
            spendable: data.vanilla.spendable,
        },
        colored: BtcBalance {
            settled: data.colored.settled,
            future: data.colored.future,
            spendable: data.colored.spendable,
        },
    }))
}

pub(crate) async fn check_indexer_url(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<CheckIndexerUrlRequest>, APIError>,
) -> Result<Json<CheckIndexerUrlResponse>, APIError> {
    let data = sdk::check_indexer_url(state.clone(), payload.indexer_url).await?;
    Ok(Json(CheckIndexerUrlResponse {
        indexer_protocol: data.indexer_protocol.into(),
    }))
}

pub(crate) async fn check_proxy_endpoint(
    WithRejection(Json(payload), _): WithRejection<Json<CheckProxyEndpointRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    sdk::check_proxy_endpoint(payload.proxy_endpoint).await?;
    Ok(Json(EmptyResponse {}))
}

pub(crate) async fn estimate_fee(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<EstimateFeeRequest>, APIError>,
) -> Result<Json<EstimateFeeResponse>, APIError> {
    let data = sdk::estimate_fee(state.clone(), payload.blocks).await?;
    Ok(Json(EstimateFeeResponse {
        fee_rate: data.fee_rate,
    }))
}

pub(crate) async fn get_channel_id(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<GetChannelIdRequest>, APIError>,
) -> Result<Json<GetChannelIdResponse>, APIError> {
    let data = sdk::get_channel_id(state.clone(), payload.temporary_channel_id).await?;
    Ok(Json(GetChannelIdResponse {
        channel_id: data.channel_id,
    }))
}

pub(crate) async fn list_channels(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ListChannelsResponse>, APIError> {
    let data = sdk::list_channels(state.clone()).await?;
    let channels = data
        .into_iter()
        .map(|c| Channel {
            channel_id: c.channel_id,
            funding_txid: c.funding_txid,
            peer_pubkey: c.peer_pubkey,
            peer_alias: c.peer_alias,
            short_channel_id: c.short_channel_id,
            status: c.status.into(),
            ready: c.ready,
            capacity_sat: c.capacity_sat,
            local_balance_sat: c.local_balance_sat,
            outbound_balance_msat: c.outbound_balance_msat,
            inbound_balance_msat: c.inbound_balance_msat,
            next_outbound_htlc_limit_msat: c.next_outbound_htlc_limit_msat,
            next_outbound_htlc_minimum_msat: c.next_outbound_htlc_minimum_msat,
            is_usable: c.is_usable,
            public: c.public,
            asset_id: c.asset_id,
            asset_local_amount: c.asset_local_amount,
            asset_remote_amount: c.asset_remote_amount,
        })
        .collect();

    Ok(Json(ListChannelsResponse { channels }))
}

pub(crate) async fn list_peers(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ListPeersResponse>, APIError> {
    let data = sdk::list_peers(state.clone()).await?;
    let peers = data
        .into_iter()
        .map(|p| Peer { pubkey: p.pubkey })
        .collect();

    Ok(Json(ListPeersResponse { peers }))
}

pub(crate) async fn list_transactions(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<ListTransactionsRequest>, APIError>,
) -> Result<Json<ListTransactionsResponse>, APIError> {
    let data = sdk::list_transactions(state.clone(), payload.skip_sync).await?;
    let transactions = data
        .into_iter()
        .map(|tx| Transaction {
            transaction_type: tx.transaction_type.into(),
            txid: tx.txid,
            received: tx.received,
            sent: tx.sent,
            fee: tx.fee,
            confirmation_time: tx.confirmation_time.map(|ct| BlockTime {
                height: ct.height,
                timestamp: ct.timestamp,
            }),
        })
        .collect();

    Ok(Json(ListTransactionsResponse { transactions }))
}

pub(crate) async fn list_transfers(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<ListTransfersRequest>, APIError>,
) -> Result<Json<ListTransfersResponse>, APIError> {
    let data = sdk::list_transfers(state.clone(), payload.asset_id).await?;
    let transfers = data
        .into_iter()
        .map(|t| Transfer {
            idx: t.idx,
            created_at: t.created_at,
            updated_at: t.updated_at,
            status: t.status.into(),
            requested_assignment: t.requested_assignment.map(Into::into),
            assignments: t.assignments.into_iter().map(Into::into).collect(),
            kind: t.kind.into(),
            txid: t.txid,
            recipient_id: t.recipient_id,
            receive_utxo: t.receive_utxo,
            change_utxo: t.change_utxo,
            expiration: t.expiration,
            transport_endpoints: t
                .transport_endpoints
                .into_iter()
                .map(|tte| TransferTransportEndpoint {
                    endpoint: tte.endpoint,
                    transport_type: tte.transport_type.into(),
                    used: tte.used,
                })
                .collect(),
        })
        .collect();
    Ok(Json(ListTransfersResponse { transfers }))
}

pub(crate) async fn list_unspents(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<ListUnspentsRequest>, APIError>,
) -> Result<Json<ListUnspentsResponse>, APIError> {
    let data = sdk::list_unspents(state.clone(), payload.skip_sync).await?;
    let unspents = data
        .into_iter()
        .map(|u| Unspent {
            utxo: Utxo {
                outpoint: u.utxo.outpoint,
                btc_amount: u.utxo.btc_amount,
                colorable: u.utxo.colorable,
            },
            rgb_allocations: u
                .rgb_allocations
                .into_iter()
                .map(|a| RgbAllocation {
                    asset_id: a.asset_id,
                    assignment: a.assignment.into(),
                    settled: a.settled,
                })
                .collect(),
        })
        .collect();
    Ok(Json(ListUnspentsResponse { unspents }))
}

pub(crate) async fn network_info(
    State(state): State<Arc<AppState>>,
) -> Result<Json<NetworkInfoResponse>, APIError> {
    let data = sdk::network_info(state.clone()).await?;
    Ok(Json(NetworkInfoResponse {
        network: data.network.into(),
        height: data.height,
    }))
}

pub(crate) async fn node_info(
    State(state): State<Arc<AppState>>,
) -> Result<Json<NodeInfoResponse>, APIError> {
    let data = sdk::node_info(state.clone()).await?;

    Ok(Json(NodeInfoResponse {
        pubkey: data.pubkey,
        num_channels: data.num_channels,
        num_usable_channels: data.num_usable_channels,
        local_balance_sat: data.local_balance_sat,
        eventual_close_fees_sat: data.eventual_close_fees_sat,
        pending_outbound_payments_sat: data.pending_outbound_payments_sat,
        num_peers: data.num_peers,
        account_xpub_vanilla: data.account_xpub_vanilla,
        account_xpub_colored: data.account_xpub_colored,
        max_media_upload_size_mb: state.static_state.max_media_upload_size_mb,
        rgb_htlc_min_msat: HTLC_MIN_MSAT,
        rgb_channel_capacity_min_sat: OPENRGBCHANNEL_MIN_SAT,
        channel_capacity_min_sat: OPENCHANNEL_MIN_SAT,
        channel_capacity_max_sat: OPENCHANNEL_MAX_SAT,
        channel_asset_min_amount: OPENCHANNEL_MIN_RGB_AMT,
        channel_asset_max_amount: u64::MAX,
        network_nodes: data.network_nodes,
        network_channels: data.network_channels,
    }))
}

pub(crate) async fn sign_message(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<SignMessageRequest>, APIError>,
) -> Result<Json<SignMessageResponse>, APIError> {
    let data = sdk::sign_message(state.clone(), payload.message).await?;
    Ok(Json(SignMessageResponse {
        signed_message: data.signed_message,
    }))
}
