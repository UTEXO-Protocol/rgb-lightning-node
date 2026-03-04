use super::*;

pub(crate) async fn create_utxos(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<CreateUtxosRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        unlocked_state.rgb_create_utxos(
            payload.up_to,
            payload.num.unwrap_or(UTXO_NUM),
            payload.size.unwrap_or(UTXO_SIZE_SAT),
            payload.fee_rate,
            payload.skip_sync,
        )?;
        tracing::debug!("UTXO creation complete");

        Ok(Json(EmptyResponse {}))
    })
    .await
}

pub(crate) async fn fail_transfers(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<FailTransfersRequest>, APIError>,
) -> Result<Json<FailTransfersResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        let unlocked_state_copy = unlocked_state.clone();
        let transfers_changed = tokio::task::spawn_blocking(move || {
            unlocked_state_copy.rgb_fail_transfers(
                payload.batch_transfer_idx,
                payload.no_asset_only,
                payload.skip_sync,
            )
        })
        .await
        .unwrap()?;

        Ok(Json(FailTransfersResponse { transfers_changed }))
    })
    .await
}

pub(crate) async fn send_btc(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<SendBtcRequest>, APIError>,
) -> Result<Json<SendBtcResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        let txid = unlocked_state.rgb_send_btc(
            payload.address,
            payload.amount,
            payload.fee_rate,
            payload.skip_sync,
        )?;

        Ok(Json(SendBtcResponse { txid }))
    })
    .await
}

pub(crate) async fn sync(
    State(state): State<Arc<AppState>>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        unlocked_state.rgb_sync()?;

        Ok(Json(EmptyResponse {}))
    })
    .await
}
