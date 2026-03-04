use super::*;

pub(crate) async fn asset_balance(
    state: Arc<AppState>,
    asset_id: String,
) -> Result<AssetBalanceData, APIError> {
    let guard = state.check_unlocked().await?;
    let unlocked_state = guard.as_ref().unwrap();

    let contract_id =
        ContractId::from_str(&asset_id).map_err(|_| APIError::InvalidAssetID(asset_id))?;
    let balance = unlocked_state.rgb_get_asset_balance(contract_id)?;

    let mut offchain_outbound = 0;
    let mut offchain_inbound = 0;
    for chan_info in unlocked_state.channel_manager.list_channels() {
        let info_file_path = get_rgb_channel_info_path(
            &chan_info.channel_id.0.as_hex().to_string(),
            &state.static_state.ldk_data_dir,
            false,
        );
        if !info_file_path.exists() {
            continue;
        }
        let rgb_info = parse_rgb_channel_info(&info_file_path);
        if rgb_info.contract_id == contract_id {
            offchain_outbound += rgb_info.local_rgb_amount;
            offchain_inbound += rgb_info.remote_rgb_amount;
        }
    }

    Ok(AssetBalanceData {
        settled: balance.settled,
        future: balance.future,
        spendable: balance.spendable,
        offchain_outbound,
        offchain_inbound,
    })
}

pub(crate) async fn asset_metadata(
    state: Arc<AppState>,
    asset_id: String,
) -> Result<AssetMetadataData, APIError> {
    let contract_id =
        ContractId::from_str(&asset_id).map_err(|_| APIError::InvalidAssetID(asset_id))?;
    let metadata = state
        .check_unlocked()
        .await?
        .clone()
        .unwrap()
        .rgb_get_asset_metadata(contract_id)?;

    Ok(AssetMetadataData {
        asset_schema: metadata.asset_schema,
        initial_supply: metadata.initial_supply,
        max_supply: metadata.max_supply,
        known_circulating_supply: metadata.known_circulating_supply,
        timestamp: metadata.timestamp,
        name: metadata.name,
        precision: metadata.precision,
        ticker: metadata.ticker,
        details: metadata.details,
        token: metadata.token.map(Into::into),
    })
}

pub(crate) async fn get_asset_media(
    state: Arc<AppState>,
    digest: String,
) -> Result<AssetMediaData, APIError> {
    let file_path = state
        .check_unlocked()
        .await?
        .clone()
        .unwrap()
        .rgb_get_media_dir()
        .join(digest.to_lowercase());
    if !file_path.exists() {
        return Err(APIError::InvalidMediaDigest);
    }

    let mut buf_reader = BufReader::new(File::open(file_path).await?);
    let mut file_bytes = Vec::new();
    buf_reader.read_to_end(&mut file_bytes).await?;

    Ok(AssetMediaData {
        bytes_hex: hex_str(&file_bytes),
    })
}

pub(crate) async fn list_assets(
    state: Arc<AppState>,
    filter_asset_schemas: Vec<rgb_lib::AssetSchema>,
) -> Result<ListAssetsData, APIError> {
    let guard = state.check_unlocked().await?;
    let unlocked_state = guard.as_ref().unwrap();

    let rgb_assets = unlocked_state.rgb_list_assets(filter_asset_schemas)?;

    let mut offchain_balances = HashMap::new();
    for chan_info in unlocked_state.channel_manager.list_channels() {
        let info_file_path = get_rgb_channel_info_path(
            &chan_info.channel_id.0.as_hex().to_string(),
            &state.static_state.ldk_data_dir,
            false,
        );
        if !info_file_path.exists() {
            continue;
        }
        let rgb_info = parse_rgb_channel_info(&info_file_path);
        offchain_balances
            .entry(rgb_info.contract_id.to_string())
            .and_modify(|(offchain_outbound, offchain_inbound)| {
                *offchain_outbound += rgb_info.local_rgb_amount;
                *offchain_inbound += rgb_info.remote_rgb_amount;
            })
            .or_insert((rgb_info.local_rgb_amount, rgb_info.remote_rgb_amount));
    }

    let nia = rgb_assets.nia.map(|assets| {
        assets
            .into_iter()
            .map(|a| {
                let mut asset: AssetNIA = a.into();
                (
                    asset.balance.offchain_outbound,
                    asset.balance.offchain_inbound,
                ) = *offchain_balances.get(&asset.asset_id).unwrap_or(&(0, 0));
                asset
            })
            .collect()
    });
    let uda = rgb_assets.uda.map(|assets| {
        assets
            .into_iter()
            .map(|a| {
                let mut asset: AssetUDA = a.into();
                (
                    asset.balance.offchain_outbound,
                    asset.balance.offchain_inbound,
                ) = *offchain_balances.get(&asset.asset_id).unwrap_or(&(0, 0));
                asset
            })
            .collect()
    });
    let cfa = rgb_assets.cfa.map(|assets| {
        assets
            .into_iter()
            .map(|a| {
                let mut asset: AssetCFA = a.into();
                (
                    asset.balance.offchain_outbound,
                    asset.balance.offchain_inbound,
                ) = *offchain_balances.get(&asset.asset_id).unwrap_or(&(0, 0));
                asset
            })
            .collect()
    });

    Ok(ListAssetsData { nia, uda, cfa })
}

pub(crate) async fn send_rgb(
    state: Arc<AppState>,
    recipient_map: HashMap<String, Vec<rgb_lib::wallet::Recipient>>,
    donation: bool,
    fee_rate: u64,
    min_confirmations: u8,
    skip_sync: bool,
) -> Result<SendRgbData, APIError> {
    let guard = state.check_unlocked().await?;
    let unlocked_state = guard.as_ref().unwrap();

    if *unlocked_state.rgb_send_lock.lock().unwrap() {
        return Err(APIError::OpenChannelInProgress);
    }

    let unlocked_state_copy = unlocked_state.clone();
    let send_result = tokio::task::spawn_blocking(move || {
        unlocked_state_copy.rgb_send(
            recipient_map,
            donation,
            fee_rate,
            min_confirmations,
            skip_sync,
        )
    })
    .await
    .unwrap()?;

    Ok(SendRgbData {
        txid: send_result.txid,
        batch_transfer_idx: send_result.batch_transfer_idx,
    })
}
