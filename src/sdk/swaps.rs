use super::*;

fn map_swap(
    payment_hash: &PaymentHash,
    swap_data: &SwapData,
    taker: bool,
    state: &crate::utils::UnlockedAppState,
) -> SwapViewData {
    let mut status: SwapStatus = swap_data.status.clone().into();
    if status == SwapStatus::Waiting && get_current_timestamp() > swap_data.swap_info.expiry {
        status = SwapStatus::Expired;
    } else if status == SwapStatus::Pending
        && get_current_timestamp() > swap_data.initiated_at.unwrap() + 86400
    {
        status = SwapStatus::Failed;
    }
    let current_status: SwapStatus = swap_data.status.clone().into();
    if status != current_status {
        if taker {
            state.update_taker_swap_status(payment_hash, status.clone().into());
        } else {
            state.update_maker_swap_status(payment_hash, status.clone().into());
        }
    }

    SwapViewData {
        payment_hash: payment_hash.to_string(),
        qty_from: swap_data.swap_info.qty_from,
        qty_to: swap_data.swap_info.qty_to,
        from_asset: swap_data.swap_info.from_asset.map(|c| c.to_string()),
        to_asset: swap_data.swap_info.to_asset.map(|c| c.to_string()),
        status,
        requested_at: swap_data.requested_at,
        initiated_at: swap_data.initiated_at,
        expires_at: swap_data.swap_info.expiry,
        completed_at: swap_data.completed_at,
    }
}

pub(crate) async fn get_swap(
    state: Arc<AppState>,
    payment_hash_hex: String,
    taker: bool,
) -> Result<SwapViewData, APIError> {
    let guard = state.check_unlocked().await?;
    let unlocked_state = guard.as_ref().unwrap();

    let payment_hash_vec = hex_str_to_vec(&payment_hash_hex);
    if payment_hash_vec.is_none() || payment_hash_vec.as_ref().unwrap().len() != 32 {
        return Err(APIError::InvalidPaymentHash(payment_hash_hex));
    }
    let requested_ph = PaymentHash(payment_hash_vec.unwrap().try_into().unwrap());

    if taker {
        let taker_swaps = unlocked_state.taker_swaps();
        if let Some(sd) = taker_swaps.get(&requested_ph) {
            return Ok(map_swap(&requested_ph, sd, true, unlocked_state));
        }
    } else {
        let maker_swaps = unlocked_state.maker_swaps();
        if let Some(sd) = maker_swaps.get(&requested_ph) {
            return Ok(map_swap(&requested_ph, sd, false, unlocked_state));
        }
    }

    Err(APIError::SwapNotFound(payment_hash_hex))
}

pub(crate) async fn list_swaps(state: Arc<AppState>) -> Result<SwapListData, APIError> {
    let guard = state.check_unlocked().await?;
    let unlocked_state = guard.as_ref().unwrap();

    let taker_swaps = unlocked_state.taker_swaps();
    let maker_swaps = unlocked_state.maker_swaps();

    Ok(SwapListData {
        taker: taker_swaps
            .iter()
            .map(|(ph, sd)| map_swap(ph, sd, true, unlocked_state))
            .collect(),
        maker: maker_swaps
            .iter()
            .map(|(ph, sd)| map_swap(ph, sd, false, unlocked_state))
            .collect(),
    })
}
