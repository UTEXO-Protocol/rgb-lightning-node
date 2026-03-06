use super::*;

pub(crate) async fn get_channel_id(
    state: Arc<AppState>,
    temporary_channel_id: String,
) -> Result<ChannelIdData, APIError> {
    let tmp_chan_id = check_channel_id(&temporary_channel_id)?;
    let channel_ids = state.check_unlocked().await?.clone().unwrap().channel_ids();
    let channel_id = channel_ids
        .get(&tmp_chan_id)
        .map(|channel_id| channel_id.0.as_hex().to_string())
        .ok_or(APIError::UnknownTemporaryChannelId)?;

    Ok(ChannelIdData { channel_id })
}

pub(crate) async fn list_channels(state: Arc<AppState>) -> Result<Vec<ChannelData>, APIError> {
    let guard = state.check_unlocked().await?;
    let unlocked_state = guard.as_ref().unwrap();

    let mut channels = vec![];
    for chan_info in unlocked_state.channel_manager.list_channels() {
        let status = match chan_info.channel_shutdown_state.unwrap() {
            ChannelShutdownState::NotShuttingDown => {
                if chan_info.is_channel_ready {
                    ChannelStatus::Opened
                } else {
                    ChannelStatus::Opening
                }
            }
            _ => ChannelStatus::Closing,
        };
        let mut channel = ChannelData {
            channel_id: chan_info.channel_id.0.as_hex().to_string(),
            peer_pubkey: hex_str(&chan_info.counterparty.node_id.serialize()),
            status,
            ready: chan_info.is_channel_ready,
            capacity_sat: chan_info.channel_value_satoshis,
            local_balance_sat: 0,
            outbound_balance_msat: chan_info.outbound_capacity_msat,
            inbound_balance_msat: chan_info.inbound_capacity_msat,
            next_outbound_htlc_limit_msat: chan_info.next_outbound_htlc_limit_msat,
            next_outbound_htlc_minimum_msat: chan_info.next_outbound_htlc_minimum_msat,
            is_usable: chan_info.is_usable,
            public: chan_info.is_announced,
            funding_txid: None,
            peer_alias: None,
            short_channel_id: None,
            asset_id: None,
            asset_local_amount: None,
            asset_remote_amount: None,
        };

        if let Some(funding_txo) = chan_info.funding_txo {
            channel.funding_txid = Some(funding_txo.txid.to_string());
            if let Ok(chan_monitor) = unlocked_state
                .chain_monitor
                .get_monitor(chan_info.channel_id)
            {
                channel.local_balance_sat = chan_monitor
                    .get_claimable_balances()
                    .iter()
                    .map(|b| b.claimable_amount_satoshis())
                    .sum::<u64>();
            }
        }

        if let Some(node_info) = unlocked_state
            .network_graph
            .read_only()
            .nodes()
            .get(&NodeId::from_pubkey(&chan_info.counterparty.node_id))
        {
            if let Some(announcement) = &node_info.announcement_info {
                channel.peer_alias = Some(announcement.alias().to_string());
            }
        }

        channel.short_channel_id = chan_info.short_channel_id;

        let info_file_path = get_rgb_channel_info_path(
            &chan_info.channel_id.0.as_hex().to_string(),
            &state.static_state.ldk_data_dir,
            false,
        );
        if info_file_path.exists() {
            let rgb_info = parse_rgb_channel_info(&info_file_path);
            channel.asset_id = Some(rgb_info.contract_id.to_string());
            channel.asset_local_amount = Some(rgb_info.local_rgb_amount);
            channel.asset_remote_amount = Some(rgb_info.remote_rgb_amount);
        }

        channels.push(channel);
    }

    Ok(channels)
}

pub(crate) async fn list_peers(state: Arc<AppState>) -> Result<Vec<PeerData>, APIError> {
    let guard = state.check_unlocked().await?;
    let unlocked_state = guard.as_ref().unwrap();

    Ok(unlocked_state
        .peer_manager
        .list_peers()
        .into_iter()
        .map(|peer_details| PeerData {
            pubkey: peer_details.counterparty_node_id.to_string(),
        })
        .collect())
}
