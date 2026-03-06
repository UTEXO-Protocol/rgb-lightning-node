use super::*;

pub(crate) async fn estimate_fee(
    state: Arc<AppState>,
    blocks: u16,
) -> Result<EstimateFeeData, APIError> {
    let fee_rate = state
        .check_unlocked()
        .await?
        .clone()
        .unwrap()
        .rgb_get_fee_estimation(blocks)?;
    Ok(EstimateFeeData { fee_rate })
}

pub(crate) async fn check_indexer_url(
    state: Arc<AppState>,
    indexer_url: String,
) -> Result<CheckIndexerUrlData, APIError> {
    let indexer_protocol = rgb_lib_check_indexer_url(&indexer_url, state.static_state.network)?;
    Ok(CheckIndexerUrlData { indexer_protocol })
}

pub(crate) async fn check_proxy_endpoint(proxy_endpoint: String) -> Result<(), APIError> {
    check_rgb_proxy_endpoint(&proxy_endpoint).await?;
    Ok(())
}
