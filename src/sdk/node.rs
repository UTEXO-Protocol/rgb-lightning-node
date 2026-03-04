use super::*;

pub(crate) async fn node_info(state: Arc<AppState>) -> Result<NodeInfoData, APIError> {
    let guard = state.check_unlocked().await?;
    let unlocked_state = guard.as_ref().unwrap();

    let chans = unlocked_state.channel_manager.list_channels();

    let balances = unlocked_state.chain_monitor.get_claimable_balances(&[]);
    let local_balance_sat = balances
        .iter()
        .map(|b| b.claimable_amount_satoshis())
        .sum::<u64>();

    let close_fees_map = |b| match b {
        &Balance::ClaimableOnChannelClose {
            ref balance_candidates,
            confirmed_balance_candidate_index,
            ..
        } => balance_candidates[confirmed_balance_candidate_index].transaction_fee_satoshis,
        _ => 0,
    };
    let eventual_close_fees_sat = balances.iter().map(close_fees_map).sum::<u64>();

    let pending_payments_map = |b| match b {
        &Balance::MaybeTimeoutClaimableHTLC {
            amount_satoshis,
            outbound_payment,
            ..
        } => {
            if outbound_payment {
                amount_satoshis
            } else {
                0
            }
        }
        _ => 0,
    };
    let pending_outbound_payments_sat = balances.iter().map(pending_payments_map).sum::<u64>();

    let graph_lock = unlocked_state.network_graph.read_only();
    let network_nodes = graph_lock.nodes().len();
    let network_channels = graph_lock.channels().len();

    let wallet_data = unlocked_state.rgb_get_wallet_data();

    Ok(NodeInfoData {
        pubkey: unlocked_state.channel_manager.get_our_node_id().to_string(),
        num_channels: chans.len(),
        num_usable_channels: chans.iter().filter(|c| c.is_usable).count(),
        local_balance_sat,
        eventual_close_fees_sat,
        pending_outbound_payments_sat,
        num_peers: unlocked_state.peer_manager.list_peers().len(),
        account_xpub_vanilla: wallet_data.account_xpub_vanilla,
        account_xpub_colored: wallet_data.account_xpub_colored,
        network_nodes,
        network_channels,
    })
}

pub(crate) async fn network_info(state: Arc<AppState>) -> Result<NetworkInfoData, APIError> {
    let guard = state.check_unlocked().await?;
    let unlocked_state = guard.as_ref().unwrap();
    let best_block = unlocked_state.channel_manager.current_best_block();

    Ok(NetworkInfoData {
        network: state.static_state.network,
        height: best_block.height,
    })
}

pub(crate) async fn address(state: Arc<AppState>) -> Result<AddressData, APIError> {
    let guard = state.check_unlocked().await?;
    let unlocked_state = guard.as_ref().unwrap();

    Ok(AddressData {
        address: unlocked_state.rgb_get_address()?,
    })
}

pub(crate) async fn btc_balance(
    state: Arc<AppState>,
    skip_sync: bool,
) -> Result<BtcBalanceData, APIError> {
    let guard = state.check_unlocked().await?;
    let unlocked_state = guard.as_ref().unwrap();
    let btc_balance = unlocked_state.rgb_get_btc_balance(skip_sync)?;

    Ok(BtcBalanceData {
        vanilla: BtcBalance {
            settled: btc_balance.vanilla.settled,
            future: btc_balance.vanilla.future,
            spendable: btc_balance.vanilla.spendable,
        },
        colored: BtcBalance {
            settled: btc_balance.colored.settled,
            future: btc_balance.colored.future,
            spendable: btc_balance.colored.spendable,
        },
    })
}

pub(crate) async fn sign_message(
    state: Arc<AppState>,
    message: String,
) -> Result<SignMessageData, APIError> {
    let guard = state.check_unlocked().await?;
    let unlocked_state = guard.as_ref().unwrap();

    let trimmed = message.trim();
    let signed_message = lightning::util::message_signing::sign(
        trimmed.as_bytes(),
        &unlocked_state.keys_manager.get_node_secret_key(),
    );
    Ok(SignMessageData { signed_message })
}
