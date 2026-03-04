use super::*;

pub(crate) async fn decode_ln_invoice(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<DecodeLNInvoiceRequest>, APIError>,
) -> Result<Json<DecodeLNInvoiceResponse>, APIError> {
    let data = sdk::decode_ln_invoice(state.clone(), payload.invoice).await?;
    Ok(Json(DecodeLNInvoiceResponse {
        amt_msat: data.amt_msat,
        expiry_sec: data.expiry_sec,
        timestamp: data.timestamp,
        asset_id: data.asset_id,
        asset_amount: data.asset_amount,
        payment_hash: data.payment_hash,
        payment_secret: data.payment_secret,
        payee_pubkey: data.payee_pubkey,
        network: data.network.into(),
    }))
}

pub(crate) async fn decode_rgb_invoice(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<DecodeRGBInvoiceRequest>, APIError>,
) -> Result<Json<DecodeRGBInvoiceResponse>, APIError> {
    let data = sdk::decode_rgb_invoice(state.clone(), payload.invoice).await?;
    Ok(Json(DecodeRGBInvoiceResponse {
        recipient_id: data.recipient_id,
        recipient_type: data.recipient_type.into(),
        asset_schema: data.asset_schema.map(Into::into),
        asset_id: data.asset_id,
        assignment: data.assignment.into(),
        network: data.network.into(),
        expiration_timestamp: data.expiration_timestamp,
        transport_endpoints: data.transport_endpoints,
    }))
}

pub(crate) async fn get_payment(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<GetPaymentRequest>, APIError>,
) -> Result<Json<GetPaymentResponse>, APIError> {
    let data = sdk::get_payment(state.clone(), payload.payment_hash).await?;
    Ok(Json(GetPaymentResponse {
        payment: Payment {
            amt_msat: data.amt_msat,
            asset_amount: data.asset_amount,
            asset_id: data.asset_id,
            payment_hash: data.payment_hash,
            inbound: data.inbound,
            status: data.status.into(),
            created_at: data.created_at,
            updated_at: data.updated_at,
            payee_pubkey: data.payee_pubkey,
        },
    }))
}

pub(crate) async fn get_swap(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<GetSwapRequest>, APIError>,
) -> Result<Json<GetSwapResponse>, APIError> {
    let data = sdk::get_swap(state.clone(), payload.payment_hash, payload.taker).await?;
    Ok(Json(GetSwapResponse {
        swap: Swap {
            qty_from: data.qty_from,
            qty_to: data.qty_to,
            from_asset: data.from_asset,
            to_asset: data.to_asset,
            payment_hash: data.payment_hash,
            status: data.status.into(),
            requested_at: data.requested_at,
            initiated_at: data.initiated_at,
            expires_at: data.expires_at,
            completed_at: data.completed_at,
        },
    }))
}

pub(crate) async fn invoice_status(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<InvoiceStatusRequest>, APIError>,
) -> Result<Json<InvoiceStatusResponse>, APIError> {
    let data = sdk::invoice_status(state.clone(), payload.invoice).await?;
    Ok(Json(InvoiceStatusResponse {
        status: match data.status {
            sdk::InvoiceStatus::Pending => InvoiceStatus::Pending,
            sdk::InvoiceStatus::Succeeded => InvoiceStatus::Succeeded,
            sdk::InvoiceStatus::Failed => InvoiceStatus::Failed,
            sdk::InvoiceStatus::Expired => InvoiceStatus::Expired,
        },
    }))
}

pub(crate) async fn list_payments(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ListPaymentsResponse>, APIError> {
    let data = sdk::list_payments(state.clone()).await?;
    let payments = data
        .into_iter()
        .map(|p| Payment {
            amt_msat: p.amt_msat,
            asset_amount: p.asset_amount,
            asset_id: p.asset_id,
            payment_hash: p.payment_hash,
            inbound: p.inbound,
            status: p.status.into(),
            created_at: p.created_at,
            updated_at: p.updated_at,
            payee_pubkey: p.payee_pubkey,
        })
        .collect();

    Ok(Json(ListPaymentsResponse { payments }))
}

pub(crate) async fn list_swaps(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ListSwapsResponse>, APIError> {
    let data = sdk::list_swaps(state.clone()).await?;

    Ok(Json(ListSwapsResponse {
        taker: data
            .taker
            .iter()
            .map(|s| Swap {
                qty_from: s.qty_from,
                qty_to: s.qty_to,
                from_asset: s.from_asset.clone(),
                to_asset: s.to_asset.clone(),
                payment_hash: s.payment_hash.clone(),
                status: s.status.clone().into(),
                requested_at: s.requested_at,
                initiated_at: s.initiated_at,
                expires_at: s.expires_at,
                completed_at: s.completed_at,
            })
            .collect(),
        maker: data
            .maker
            .iter()
            .map(|s| Swap {
                qty_from: s.qty_from,
                qty_to: s.qty_to,
                from_asset: s.from_asset.clone(),
                to_asset: s.to_asset.clone(),
                payment_hash: s.payment_hash.clone(),
                status: s.status.clone().into(),
                requested_at: s.requested_at,
                initiated_at: s.initiated_at,
                expires_at: s.expires_at,
                completed_at: s.completed_at,
            })
            .collect(),
    }))
}
