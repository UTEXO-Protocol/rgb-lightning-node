use super::*;

pub(crate) async fn keysend(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<KeysendRequest>, APIError>,
) -> Result<Json<KeysendResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        let dest_pubkey = match hex_str_to_compressed_pubkey(&payload.dest_pubkey) {
            Some(pk) => pk,
            None => return Err(APIError::InvalidPubkey),
        };

        let amt_msat = payload.amt_msat;
        if amt_msat < HTLC_MIN_MSAT {
            return Err(APIError::InvalidAmount(format!(
                "amt_msat cannot be less than {HTLC_MIN_MSAT}"
            )));
        }

        let payment_preimage =
            PaymentPreimage(unlocked_state.keys_manager.get_secure_random_bytes());
        let payment_hash_inner = Sha256::hash(&payment_preimage.0[..]).to_byte_array();
        let payment_id = PaymentId(payment_hash_inner);
        let payment_hash = PaymentHash(payment_hash_inner);

        let rgb_payment = match (payload.asset_id, payload.asset_amount) {
            (Some(asset_id), Some(rgb_amount)) => {
                let contract_id = ContractId::from_str(&asset_id)
                    .map_err(|_| APIError::InvalidAssetID(asset_id))?;
                Some((contract_id, rgb_amount))
            }
            (None, None) => None,
            _ => {
                return Err(APIError::IncompleteRGBInfo);
            }
        };

        let route_params = RouteParameters::from_payment_params_and_value(
            PaymentParameters::for_keysend(dest_pubkey, 40, false),
            amt_msat,
            rgb_payment,
        );
        let created_at = get_current_timestamp();
        unlocked_state.add_outbound_payment(
            payment_id,
            PaymentInfo {
                preimage: None,
                secret: None,
                status: HTLCStatus::Pending.into(),
                amt_msat: Some(amt_msat),
                created_at,
                updated_at: created_at,
                payee_pubkey: dest_pubkey,
            },
        )?;
        if let Some((contract_id, rgb_amount)) = rgb_payment {
            write_rgb_payment_info_file(
                &PathBuf::from(&state.static_state.ldk_data_dir),
                &payment_hash,
                contract_id,
                rgb_amount,
                false,
                false,
            );
        }

        let status = match unlocked_state.channel_manager.send_spontaneous_payment(
            Some(payment_preimage),
            RecipientOnionFields::spontaneous_empty(),
            payment_id,
            route_params,
            Retry::Timeout(Duration::from_secs(10)),
        ) {
            Ok(_payment_hash) => {
                tracing::info!(
                    "EVENT: initiated sending {} msats to {}",
                    amt_msat,
                    dest_pubkey
                );
                HTLCStatus::Pending
            }
            Err(e) => {
                tracing::error!("ERROR: failed to send payment: {:?}", e);
                unlocked_state
                    .update_outbound_payment_status(payment_id, HTLCStatus::Failed.into());
                HTLCStatus::Failed
            }
        };

        Ok(Json(KeysendResponse {
            payment_hash: hex_str(&payment_hash.0),
            payment_preimage: hex_str(&payment_preimage.0),
            status,
        }))
    })
    .await
}

pub(crate) async fn ln_invoice(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<LNInvoiceRequest>, APIError>,
) -> Result<Json<LNInvoiceResponse>, APIError> {
    no_cancel(async move {
        let data = sdk::create_ln_invoice(
            state.clone(),
            payload.amt_msat,
            payload.expiry_sec,
            payload.asset_id,
            payload.asset_amount,
            INVOICE_MIN_MSAT,
        )
        .await?;

        Ok(Json(LNInvoiceResponse {
            invoice: data.invoice,
        }))
    })
    .await
}

pub(crate) async fn send_payment(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<SendPaymentRequest>, APIError>,
) -> Result<Json<SendPaymentResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        let mut status = HTLCStatus::Pending;
        let created_at = get_current_timestamp();

        let (payment_id, payment_hash, payment_secret) = if let Ok(offer) = Offer::from_str(&payload.invoice) {
            let random_bytes = unlocked_state.keys_manager.get_secure_random_bytes();
            let payment_id = PaymentId(random_bytes);

            let amt_msat = match (offer.amount(), payload.amt_msat) {
                (Some(offer::Amount::Bitcoin { amount_msats }), _) => amount_msats,
                (_, Some(amt)) => amt,
                (amt, _) => {
                    return Err(APIError::InvalidAmount(format!(
                        "cannot process non-Bitcoin-denominated offer value {amt:?}"
                    )));
                },
            };
            if payload.amt_msat.is_some() && payload.amt_msat != Some(amt_msat) {
                return Err(APIError::InvalidAmount(format!(
                    "amount didn't match offer of {amt_msat}msat"
                )));
            }

            // TODO: add and check RGB amount after enabling RGB support for offers

            let secret = None;

            unlocked_state.add_outbound_payment(
                payment_id,
                PaymentInfo {
                    preimage: None,
                    secret,
                    status: status.into(),
                    amt_msat: Some(amt_msat),
                    created_at,
                    updated_at: created_at,
                    payee_pubkey: offer.issuer_signing_pubkey().ok_or(APIError::InvalidInvoice(s!("missing signing pubkey")))?,
                },
            )?;

            let params = OptionalOfferPaymentParams {
                retry_strategy: Retry::Timeout(Duration::from_secs(10)),
                ..Default::default()
            };
            let pay = unlocked_state.channel_manager
                .pay_for_offer(&offer, Some(amt_msat), payment_id, params);
            if pay.is_err() {
                tracing::error!("ERROR: failed to pay: {:?}", pay);
                unlocked_state.update_outbound_payment_status(payment_id, HTLCStatus::Failed.into());
                status = HTLCStatus::Failed;
                unlocked_state.update_outbound_payment_status(payment_id, status.into());
            }
            (payment_id, None, secret)
        } else {
            let invoice = match Bolt11Invoice::from_str(&payload.invoice) {
                Err(e) => return Err(APIError::InvalidInvoice(e.to_string())),
                Ok(v) => v,
            };

            let payment_id = PaymentId((*invoice.payment_hash()).to_byte_array());
            let payment_secret = Some(*invoice.payment_secret());
            let zero_amt_invoice =
                invoice.amount_milli_satoshis().is_none() || invoice.amount_milli_satoshis() == Some(0);

            let amt_msat = if zero_amt_invoice {
                if let Some(amt_msat) = payload.amt_msat {
                    amt_msat
                } else {
                    return Err(APIError::InvalidAmount(s!(
                        "need an amount for the given 0-value invoice"
                    )));
                }
            } else {
                if payload.amt_msat.is_some() && invoice.amount_milli_satoshis() != payload.amt_msat
                {
                    return Err(APIError::InvalidAmount(format!(
                        "amount didn't match invoice value of {}msat", invoice.amount_milli_satoshis().unwrap_or(0)
                    )));
                }
                invoice.amount_milli_satoshis().unwrap_or(0)
            };

            let rgb_payment = match (invoice.rgb_contract_id(), invoice.rgb_amount()) {
                (Some(rgb_contract_id), Some(rgb_amount)) => {
                    if amt_msat < INVOICE_MIN_MSAT {
                        return Err(APIError::InvalidAmount(format!(
                            "msat amount in invoice sending an RGB asset cannot be less than {INVOICE_MIN_MSAT}"
                        )));
                    }
                    Some((rgb_contract_id, rgb_amount))
                },
                (None, None) => None,
                (Some(_), None) => {
                    return Err(APIError::InvalidInvoice(s!(
                        "invoice has an RGB contract ID but not an RGB amount"
                    )))
                }
                (None, Some(_)) => {
                    return Err(APIError::InvalidInvoice(s!(
                        "invoice has an RGB amount but not an RGB contract ID"
                    )))
                }
            };

            let secret = payment_secret;
            unlocked_state.add_outbound_payment(
                payment_id,
                PaymentInfo {
                    preimage: None,
                    secret,
                    status: status.into(),
                    amt_msat: invoice.amount_milli_satoshis(),
                    created_at,
                    updated_at: created_at,
                    payee_pubkey: invoice.get_payee_pub_key(),
                },
            )?;
            let payment_hash = PaymentHash(invoice.payment_hash().to_byte_array());
            if let Some((contract_id, rgb_amount)) = rgb_payment {
                write_rgb_payment_info_file(
                    &PathBuf::from(&state.static_state.ldk_data_dir),
                    &payment_hash,
                    contract_id,
                    rgb_amount,
                    false,
                    false,
                );
            }

            match unlocked_state.channel_manager.pay_for_bolt11_invoice(
                &invoice,
                payment_id,
                Some(amt_msat),
                RouteParametersConfig::default(),
                Retry::Timeout(Duration::from_secs(10)),
            ) {
                Ok(_) => {
                    let payee_pubkey = invoice.recover_payee_pub_key();
                    tracing::info!(
                        "EVENT: initiated sending {} msats to {}",
                        amt_msat,
                        payee_pubkey
                    );
                },
                Err(e) => {
                    tracing::error!("ERROR: failed to send payment: {:?}", e);
                    status = HTLCStatus::Failed;
                    unlocked_state.update_outbound_payment_status(payment_id, status.into());
                },
            };

            (payment_id, Some(payment_hash), secret)
        };

        Ok(Json(SendPaymentResponse {
            payment_id: hex_str(&payment_id.0),
            payment_hash: payment_hash.map(|h| hex_str(&h.0)),
            payment_secret: payment_secret.map(|s| hex_str(&s.0)),
            status,
        }))
    })
    .await
}
