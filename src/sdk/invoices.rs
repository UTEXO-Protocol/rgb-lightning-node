use super::*;

pub(crate) async fn decode_ln_invoice(
    state: Arc<AppState>,
    invoice: String,
) -> Result<DecodeLnInvoiceData, APIError> {
    let _guard = state.get_unlocked_app_state();
    let invoice =
        Bolt11Invoice::from_str(&invoice).map_err(|e| APIError::InvalidInvoice(e.to_string()))?;

    Ok(DecodeLnInvoiceData {
        amt_msat: invoice.amount_milli_satoshis(),
        expiry_sec: invoice.expiry_time().as_secs(),
        timestamp: invoice.duration_since_epoch().as_secs(),
        asset_id: invoice.rgb_contract_id().map(|c| c.to_string()),
        asset_amount: invoice.rgb_amount(),
        payment_hash: hex_str(&invoice.payment_hash().to_byte_array()),
        payment_secret: hex_str(&invoice.payment_secret().0),
        payee_pubkey: invoice.payee_pub_key().map(|p| p.to_string()),
        network: match invoice.network() {
            bitcoin::Network::Bitcoin => rgb_lib::BitcoinNetwork::Mainnet,
            bitcoin::Network::Testnet => rgb_lib::BitcoinNetwork::Testnet,
            bitcoin::Network::Testnet4 => rgb_lib::BitcoinNetwork::Testnet4,
            bitcoin::Network::Signet => rgb_lib::BitcoinNetwork::Signet,
            bitcoin::Network::Regtest => rgb_lib::BitcoinNetwork::Regtest,
            _ => return Err(APIError::InvalidInvoice("unsupported network".to_string())),
        },
    })
}

pub(crate) async fn decode_rgb_invoice(
    state: Arc<AppState>,
    invoice: String,
) -> Result<DecodeRgbInvoiceData, APIError> {
    let _guard = state.get_unlocked_app_state();
    let invoice_data = RgbLibInvoice::new(invoice)?.invoice_data();
    let recipient_info = RecipientInfo::new(invoice_data.recipient_id.clone())?;

    Ok(DecodeRgbInvoiceData {
        recipient_id: invoice_data.recipient_id,
        recipient_type: recipient_info.recipient_type,
        asset_schema: invoice_data.asset_schema,
        asset_id: invoice_data.asset_id,
        assignment: invoice_data.assignment,
        network: invoice_data.network,
        expiration_timestamp: invoice_data.expiration_timestamp,
        transport_endpoints: invoice_data.transport_endpoints,
    })
}

pub(crate) async fn invoice_status(
    state: Arc<AppState>,
    invoice: String,
) -> Result<InvoiceStatusData, APIError> {
    let guard = state.check_unlocked().await?;
    let unlocked_state = guard.as_ref().unwrap();

    let invoice =
        Bolt11Invoice::from_str(&invoice).map_err(|e| APIError::InvalidInvoice(e.to_string()))?;
    let payment_hash = PaymentHash(invoice.payment_hash().to_byte_array());
    let status = match unlocked_state.inbound_payments().get(&payment_hash) {
        Some(v) => match v.status {
            HtlcStatus::Pending if invoice.is_expired() => InvoiceStatus::Expired,
            HtlcStatus::Pending => InvoiceStatus::Pending,
            HtlcStatus::Succeeded => InvoiceStatus::Succeeded,
            HtlcStatus::Failed => InvoiceStatus::Failed,
        },
        None => return Err(APIError::UnknownLNInvoice),
    };

    Ok(InvoiceStatusData { status })
}

pub(crate) async fn create_ln_invoice(
    state: Arc<AppState>,
    amt_msat: Option<u64>,
    expiry_sec: u32,
    asset_id: Option<String>,
    asset_amount: Option<u64>,
    invoice_min_msat: u64,
) -> Result<LnInvoiceData, APIError> {
    let guard = state.check_unlocked().await?;
    let unlocked_state = guard.as_ref().unwrap();

    let contract_id = if let Some(asset_id) = asset_id {
        Some(ContractId::from_str(&asset_id).map_err(|_| APIError::InvalidAssetID(asset_id))?)
    } else {
        None
    };

    if contract_id.is_some() && amt_msat.unwrap_or(0) < invoice_min_msat {
        return Err(APIError::InvalidAmount(format!(
            "amt_msat cannot be less than {invoice_min_msat} when transferring an RGB asset"
        )));
    }

    let invoice_params = Bolt11InvoiceParameters {
        amount_msats: amt_msat,
        invoice_expiry_delta_secs: Some(expiry_sec),
        contract_id,
        asset_amount,
        ..Default::default()
    };

    let invoice = match unlocked_state
        .channel_manager
        .create_bolt11_invoice(invoice_params)
    {
        Ok(inv) => inv,
        Err(e) => return Err(APIError::FailedInvoiceCreation(e.to_string())),
    };

    let payment_hash = PaymentHash((*invoice.payment_hash()).to_byte_array());
    let created_at = get_current_timestamp();
    unlocked_state.add_inbound_payment(
        payment_hash,
        PaymentInfo {
            preimage: None,
            secret: Some(*invoice.payment_secret()),
            status: HtlcStatus::Pending,
            amt_msat,
            created_at,
            updated_at: created_at,
            payee_pubkey: unlocked_state.channel_manager.get_our_node_id(),
        },
    );

    Ok(LnInvoiceData {
        invoice: invoice.to_string(),
    })
}
