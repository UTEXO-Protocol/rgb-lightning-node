use super::*;

pub(crate) async fn asset_balance(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<AssetBalanceRequest>, APIError>,
) -> Result<Json<AssetBalanceResponse>, APIError> {
    let data = sdk::asset_balance(state.clone(), payload.asset_id).await?;
    Ok(Json(AssetBalanceResponse {
        settled: data.settled,
        future: data.future,
        spendable: data.spendable,
        offchain_outbound: data.offchain_outbound,
        offchain_inbound: data.offchain_inbound,
    }))
}

pub(crate) async fn asset_metadata(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<AssetMetadataRequest>, APIError>,
) -> Result<Json<AssetMetadataResponse>, APIError> {
    let data = sdk::asset_metadata(state.clone(), payload.asset_id).await?;
    Ok(Json(AssetMetadataResponse {
        asset_schema: data.asset_schema.into(),
        initial_supply: data.initial_supply,
        max_supply: data.max_supply,
        known_circulating_supply: data.known_circulating_supply,
        timestamp: data.timestamp,
        name: data.name,
        precision: data.precision,
        ticker: data.ticker,
        details: data.details,
        token: data.token.map(Into::into),
    }))
}

pub(crate) async fn get_asset_media(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<GetAssetMediaRequest>, APIError>,
) -> Result<Json<GetAssetMediaResponse>, APIError> {
    let data = sdk::get_asset_media(state.clone(), payload.digest).await?;
    Ok(Json(GetAssetMediaResponse {
        bytes_hex: data.bytes_hex,
    }))
}

pub(crate) async fn issue_asset_cfa(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<IssueAssetCFARequest>, APIError>,
) -> Result<Json<IssueAssetCFAResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        if *unlocked_state.rgb_send_lock.lock().unwrap() {
            return Err(APIError::OpenChannelInProgress);
        }

        let file_path = payload.file_digest.map(|d: String| {
            unlocked_state
                .rgb_get_media_dir()
                .join(d.to_lowercase())
                .to_string_lossy()
                .to_string()
        });

        let asset = unlocked_state.rgb_issue_asset_cfa(
            payload.name,
            payload.details,
            payload.precision,
            payload.amounts,
            file_path,
        )?;

        Ok(Json(IssueAssetCFAResponse {
            asset: asset.into(),
        }))
    })
    .await
}

pub(crate) async fn issue_asset_nia(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<IssueAssetNIARequest>, APIError>,
) -> Result<Json<IssueAssetNIAResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        if *unlocked_state.rgb_send_lock.lock().unwrap() {
            return Err(APIError::OpenChannelInProgress);
        }

        let asset = unlocked_state.rgb_issue_asset_nia(
            payload.ticker,
            payload.name,
            payload.precision,
            payload.amounts,
        )?;

        Ok(Json(IssueAssetNIAResponse {
            asset: asset.into(),
        }))
    })
    .await
}

pub(crate) async fn issue_asset_uda(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<IssueAssetUDARequest>, APIError>,
) -> Result<Json<IssueAssetUDAResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        if *unlocked_state.rgb_send_lock.lock().unwrap() {
            return Err(APIError::OpenChannelInProgress);
        }

        let rgb_media_dir = unlocked_state.rgb_get_media_dir();
        let get_string_path = |d: String| {
            rgb_media_dir
                .join(d.to_lowercase())
                .to_string_lossy()
                .to_string()
        };
        let media_file_path = payload.media_file_digest.map(get_string_path);
        let attachments_file_paths = payload
            .attachments_file_digests
            .into_iter()
            .map(get_string_path)
            .collect();

        let asset = unlocked_state.rgb_issue_asset_uda(
            payload.ticker,
            payload.name,
            payload.details,
            payload.precision,
            media_file_path,
            attachments_file_paths,
        )?;

        Ok(Json(IssueAssetUDAResponse {
            asset: asset.into(),
        }))
    })
    .await
}

pub(crate) async fn list_assets(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<ListAssetsRequest>, APIError>,
) -> Result<Json<ListAssetsResponse>, APIError> {
    let data = sdk::list_assets(
        state.clone(),
        payload
            .filter_asset_schemas
            .into_iter()
            .map(Into::into)
            .collect(),
    )
    .await?;
    Ok(Json(ListAssetsResponse {
        nia: data
            .nia
            .map(|assets| assets.into_iter().map(Into::into).collect()),
        uda: data
            .uda
            .map(|assets| assets.into_iter().map(Into::into).collect()),
        cfa: data
            .cfa
            .map(|assets| assets.into_iter().map(Into::into).collect()),
    }))
}

pub(crate) async fn post_asset_media(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<PostAssetMediaResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        let digest = if let Some(field) = multipart
            .next_field()
            .await
            .map_err(|_| APIError::MediaFileNotProvided)?
        {
            let file_bytes = field
                .bytes()
                .await
                .map_err(|e| APIError::Unexpected(format!("Failed to read bytes: {e}")))?;
            if file_bytes.is_empty() {
                return Err(APIError::MediaFileEmpty);
            }
            let file_hash: sha256::Hash = Hash::hash(&file_bytes[..]);
            let digest = file_hash.to_string();

            let file_path = unlocked_state.rgb_get_media_dir().join(&digest);
            let mut write = true;
            if file_path.exists() {
                let mut buf_reader = BufReader::new(File::open(&file_path).await?);
                let mut existing_file_bytes = Vec::new();
                buf_reader.read_to_end(&mut existing_file_bytes).await?;
                if file_bytes != existing_file_bytes {
                    tokio::fs::remove_file(&file_path).await?;
                } else {
                    write = false;
                }
            }
            if write {
                let mut file = File::create(&file_path).await?;
                file.write_all(&file_bytes).await?;
            }
            digest
        } else {
            return Err(APIError::MediaFileNotProvided);
        };

        Ok(Json(PostAssetMediaResponse { digest }))
    })
    .await
}

pub(crate) async fn rgb_invoice(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<RgbInvoiceRequest>, APIError>,
) -> Result<Json<RgbInvoiceResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        if *unlocked_state.rgb_send_lock.lock().unwrap() {
            return Err(APIError::OpenChannelInProgress);
        }

        let assignment = payload.assignment.unwrap_or(Assignment::Any).into();

        let receive_data = if payload.witness {
            unlocked_state.rgb_witness_receive(
                payload.asset_id,
                assignment,
                payload.duration_seconds,
                vec![unlocked_state.proxy_endpoint.clone()],
                payload.min_confirmations,
            )?
        } else {
            unlocked_state.rgb_blind_receive(
                payload.asset_id,
                assignment,
                payload.duration_seconds,
                vec![unlocked_state.proxy_endpoint.clone()],
                payload.min_confirmations,
            )?
        };

        Ok(Json(RgbInvoiceResponse {
            recipient_id: receive_data.recipient_id,
            invoice: receive_data.invoice,
            expiration_timestamp: receive_data.expiration_timestamp,
            batch_transfer_idx: receive_data.batch_transfer_idx,
        }))
    })
    .await
}

pub(crate) async fn send_rgb(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<SendRgbRequest>, APIError>,
) -> Result<Json<SendRgbResponse>, APIError> {
    no_cancel(async move {
        let recipient_map: HashMap<String, Vec<RgbLibRecipient>> = payload
            .recipient_map
            .into_iter()
            .map(|(asset_id, recipients)| {
                (asset_id, recipients.into_iter().map(|r| r.into()).collect())
            })
            .collect();

        let send_result = sdk::send_rgb(
            state.clone(),
            recipient_map,
            payload.donation,
            payload.fee_rate,
            payload.min_confirmations,
            payload.skip_sync,
        )
        .await?;
        let _batch_transfer_idx = send_result.batch_transfer_idx;

        Ok(Json(SendRgbResponse {
            txid: send_result.txid,
        }))
    })
    .await
}

pub(crate) async fn refresh_transfers(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<RefreshRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();
        let unlocked_state_copy = unlocked_state.clone();

        tokio::task::spawn_blocking(move || unlocked_state_copy.rgb_refresh(payload.skip_sync))
            .await
            .unwrap()?;

        tracing::info!("Refresh complete");
        Ok(Json(EmptyResponse {}))
    })
    .await
}
