use super::*;

pub(crate) async fn list_payments(state: Arc<AppState>) -> Result<Vec<PaymentData>, APIError> {
    let guard = state.check_unlocked().await?;
    let unlocked_state = guard.as_ref().unwrap();

    let inbound_payments = unlocked_state.inbound_payments();
    let outbound_payments = unlocked_state.outbound_payments();
    let mut payments = vec![];

    for (payment_hash, payment_info) in &inbound_payments {
        let rgb_payment_info_path_inbound =
            get_rgb_payment_info_path(payment_hash, &state.static_state.ldk_data_dir, true);

        let (asset_amount, asset_id) = if rgb_payment_info_path_inbound.exists() {
            let info = parse_rgb_payment_info(&rgb_payment_info_path_inbound);
            (Some(info.amount), Some(info.contract_id.to_string()))
        } else {
            (None, None)
        };

        payments.push(PaymentData {
            amt_msat: payment_info.amt_msat,
            asset_amount,
            asset_id,
            payment_hash: hex_str(&payment_hash.0),
            inbound: true,
            status: payment_info.status.into(),
            created_at: payment_info.created_at,
            updated_at: payment_info.updated_at,
            payee_pubkey: payment_info.payee_pubkey.to_string(),
        });
    }

    for (payment_id, payment_info) in &outbound_payments {
        let payment_hash = &PaymentHash(payment_id.0);

        let rgb_payment_info_path_outbound =
            get_rgb_payment_info_path(payment_hash, &state.static_state.ldk_data_dir, false);

        let (asset_amount, asset_id) = if rgb_payment_info_path_outbound.exists() {
            let info = parse_rgb_payment_info(&rgb_payment_info_path_outbound);
            (Some(info.amount), Some(info.contract_id.to_string()))
        } else {
            (None, None)
        };

        payments.push(PaymentData {
            amt_msat: payment_info.amt_msat,
            asset_amount,
            asset_id,
            payment_hash: hex_str(&payment_hash.0),
            inbound: false,
            status: payment_info.status.into(),
            created_at: payment_info.created_at,
            updated_at: payment_info.updated_at,
            payee_pubkey: payment_info.payee_pubkey.to_string(),
        });
    }

    Ok(payments)
}

pub(crate) async fn get_payment(
    state: Arc<AppState>,
    payment_hash_hex: String,
) -> Result<PaymentData, APIError> {
    let guard = state.check_unlocked().await?;
    let unlocked_state = guard.as_ref().unwrap();

    let payment_hash_vec = hex_str_to_vec(&payment_hash_hex);
    if payment_hash_vec.is_none() || payment_hash_vec.as_ref().unwrap().len() != 32 {
        return Err(APIError::InvalidPaymentHash(payment_hash_hex));
    }
    let requested_ph = PaymentHash(payment_hash_vec.unwrap().try_into().unwrap());

    let inbound_payments = unlocked_state.inbound_payments();
    let outbound_payments = unlocked_state.outbound_payments();

    for (payment_hash, payment_info) in &inbound_payments {
        if payment_hash == &requested_ph {
            let rgb_payment_info_path_inbound =
                get_rgb_payment_info_path(payment_hash, &state.static_state.ldk_data_dir, true);

            let (asset_amount, asset_id) = if rgb_payment_info_path_inbound.exists() {
                let info = parse_rgb_payment_info(&rgb_payment_info_path_inbound);
                (Some(info.amount), Some(info.contract_id.to_string()))
            } else {
                (None, None)
            };

            return Ok(PaymentData {
                amt_msat: payment_info.amt_msat,
                asset_amount,
                asset_id,
                payment_hash: hex_str(&payment_hash.0),
                inbound: true,
                status: payment_info.status.into(),
                created_at: payment_info.created_at,
                updated_at: payment_info.updated_at,
                payee_pubkey: payment_info.payee_pubkey.to_string(),
            });
        }
    }

    for (payment_id, payment_info) in &outbound_payments {
        let payment_hash = &PaymentHash(payment_id.0);
        if payment_hash == &requested_ph {
            let rgb_payment_info_path_outbound =
                get_rgb_payment_info_path(payment_hash, &state.static_state.ldk_data_dir, false);

            let (asset_amount, asset_id) = if rgb_payment_info_path_outbound.exists() {
                let info = parse_rgb_payment_info(&rgb_payment_info_path_outbound);
                (Some(info.amount), Some(info.contract_id.to_string()))
            } else {
                (None, None)
            };

            return Ok(PaymentData {
                amt_msat: payment_info.amt_msat,
                asset_amount,
                asset_id,
                payment_hash: hex_str(&payment_hash.0),
                inbound: false,
                status: payment_info.status.into(),
                created_at: payment_info.created_at,
                updated_at: payment_info.updated_at,
                payee_pubkey: payment_info.payee_pubkey.to_string(),
            });
        }
    }

    Err(APIError::PaymentNotFound(payment_hash_hex))
}
