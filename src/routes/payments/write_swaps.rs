use super::*;

pub(crate) async fn maker_execute(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<MakerExecuteRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        let swapstring = SwapString::from_str(&payload.swapstring)
            .map_err(|e| APIError::InvalidSwapString(payload.swapstring.clone(), e.to_string()))?;
        let payment_secret = hex_str_to_vec(&payload.payment_secret)
            .and_then(|data| data.try_into().ok())
            .map(PaymentSecret)
            .ok_or(APIError::InvalidPaymentSecret)?;
        let taker_pk =
            PublicKey::from_str(&payload.taker_pubkey).map_err(|_| APIError::InvalidPubkey)?;

        if get_current_timestamp() > swapstring.swap_info.expiry {
            unlocked_state
                .update_maker_swap_status(&swapstring.payment_hash, SwapStatus::Expired.into());
            return Err(APIError::ExpiredSwapOffer);
        }

        let payment_preimage = unlocked_state
            .channel_manager
            .get_payment_preimage(swapstring.payment_hash, payment_secret)
            .map_err(|_| APIError::MissingSwapPaymentPreimage)?;

        let swap_info = swapstring.swap_info;

        let receive_hints = unlocked_state
            .channel_manager
            .list_usable_channels()
            .iter()
            .filter(|details| {
                match get_rgb_channel_info_optional(
                    &details.channel_id,
                    &state.static_state.ldk_data_dir,
                    false,
                ) {
                    _ if swap_info.is_from_btc() => true,
                    Some((rgb_info, _)) if Some(rgb_info.contract_id) == swap_info.from_asset => {
                        true
                    }
                    _ => false,
                }
            })
            .map(|details| {
                let config = details.counterparty.forwarding_info.as_ref().unwrap();
                RouteHint(vec![RouteHintHop {
                    src_node_id: details.counterparty.node_id,
                    short_channel_id: details.short_channel_id.unwrap(),
                    cltv_expiry_delta: config.cltv_expiry_delta,
                    htlc_maximum_msat: None,
                    htlc_minimum_msat: None,
                    fees: RoutingFees {
                        base_msat: config.fee_base_msat,
                        proportional_millionths: config.fee_proportional_millionths,
                    },
                    htlc_maximum_rgb: None,
                }])
            })
            .collect();

        let rgb_payment = swap_info
            .to_asset
            .map(|to_asset| (to_asset, swap_info.qty_to));
        let first_leg = get_route(
            &unlocked_state.channel_manager,
            &unlocked_state.router,
            unlocked_state.channel_manager.get_our_node_id(),
            taker_pk,
            if swap_info.is_to_btc() {
                Some(swap_info.qty_to + HTLC_MIN_MSAT)
            } else {
                Some(HTLC_MIN_MSAT)
            },
            rgb_payment,
            vec![],
        );

        let rgb_payment = swap_info
            .from_asset
            .map(|from_asset| (from_asset, swap_info.qty_from));
        let second_leg = get_route(
            &unlocked_state.channel_manager,
            &unlocked_state.router,
            taker_pk,
            unlocked_state.channel_manager.get_our_node_id(),
            if swap_info.is_to_btc() || swap_info.is_asset_asset() {
                Some(HTLC_MIN_MSAT)
            } else {
                Some(swap_info.qty_from + HTLC_MIN_MSAT)
            },
            rgb_payment,
            receive_hints,
        );

        let (mut first_leg, mut second_leg) = match (first_leg, second_leg) {
            (Some(f), Some(s)) => (f, s),
            _ => {
                return Err(APIError::NoRoute);
            }
        };

        // Set swap flag
        second_leg.paths[0].hops[0].short_channel_id |= IS_SWAP_SCID;

        // Generally in the last hop the fee_amount is set to the payment amount, so we need to
        // override it with fee = 0
        first_leg.paths[0]
            .hops
            .last_mut()
            .expect("Path not to be empty")
            .fee_msat = 0;

        let fullpaths = first_leg.paths[0]
            .hops
            .clone()
            .into_iter()
            .map(|mut hop| {
                if swap_info.is_to_asset() {
                    hop.rgb_payment = Some((swap_info.to_asset.unwrap(), swap_info.qty_to));
                }
                hop
            })
            .chain(second_leg.paths[0].hops.clone().into_iter().map(|mut hop| {
                if swap_info.is_from_asset() {
                    hop.rgb_payment = Some((swap_info.from_asset.unwrap(), swap_info.qty_from));
                }
                hop
            }))
            .collect::<Vec<_>>();

        // Skip last fee because it's equal to the payment amount
        let total_fee = fullpaths
            .iter()
            .rev()
            .skip(1)
            .map(|hop| hop.fee_msat)
            .sum::<u64>();

        if total_fee >= MAX_SWAP_FEE_MSAT {
            return Err(APIError::FailedPayment(format!(
                "Fee too high: {total_fee}"
            )));
        }

        let route = Route {
            paths: vec![LnPath {
                hops: fullpaths,
                blinded_tail: None,
            }],
            route_params: Some(RouteParameters {
                payment_params: PaymentParameters::for_keysend(
                    unlocked_state.channel_manager.get_our_node_id(),
                    DEFAULT_FINAL_CLTV_EXPIRY_DELTA,
                    false,
                ),
                // This value is not used anywhere, it's set by the router
                // when creating a route, but here we are creating it manually
                // by composing a pre-existing list of hops
                final_value_msat: 0,
                max_total_routing_fee_msat: None,
                // This value is not used anywhere, same as final_value_msat
                rgb_payment: None,
            }),
        };

        if swap_info.is_to_asset() {
            write_rgb_payment_info_file(
                &state.static_state.ldk_data_dir,
                &swapstring.payment_hash,
                swap_info.to_asset.unwrap(),
                swap_info.qty_to,
                true,
                false,
            );
        }

        unlocked_state
            .update_maker_swap_status(&swapstring.payment_hash, SwapStatus::Pending.into());

        let payment_hash: PaymentHash = payment_preimage.into();
        let (_status, err) = match unlocked_state
            .channel_manager
            .send_spontaneous_payment_with_route(
                route,
                payment_hash,
                payment_preimage,
                RecipientOnionFields::spontaneous_empty(),
                PaymentId(swapstring.payment_hash.0),
            ) {
            Ok(()) => {
                tracing::debug!("EVENT: initiated swap");
                (HTLCStatus::Pending, None)
            }
            Err(e) => {
                tracing::warn!("ERROR: failed to send payment: {:?}", e);
                (HTLCStatus::Failed, Some(e))
            }
        };

        match err {
            None => Ok(Json(EmptyResponse {})),
            Some(e) => {
                unlocked_state
                    .update_maker_swap_status(&swapstring.payment_hash, SwapStatus::Failed.into());
                Err(APIError::FailedPayment(format!("{e:?}")))
            }
        }
    })
    .await
}

pub(crate) async fn maker_init(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<MakerInitRequest>, APIError>,
) -> Result<Json<MakerInitResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        let from_asset = match &payload.from_asset {
            None => None,
            Some(asset) => Some(
                ContractId::from_str(asset).map_err(|_| APIError::InvalidAssetID(asset.clone()))?,
            ),
        };

        let to_asset = match &payload.to_asset {
            None => None,
            Some(asset) => Some(
                ContractId::from_str(asset).map_err(|_| APIError::InvalidAssetID(asset.clone()))?,
            ),
        };

        // prevent BTC-to-BTC swaps
        if from_asset.is_none() && to_asset.is_none() {
            return Err(APIError::InvalidSwap(s!("cannot swap BTC for BTC")));
        }

        // prevent swaps of same assets
        if from_asset == to_asset {
            return Err(APIError::InvalidSwap(s!("cannot swap the same asset")));
        }

        let qty_from = payload.qty_from;
        let qty_to = payload.qty_to;

        let expiry = get_current_timestamp() + payload.timeout_sec as u64;
        let swap_info = SwapInfo {
            from_asset,
            to_asset,
            qty_from,
            qty_to,
            expiry,
        };
        let swap_data = SwapData::create_from_swap_info(&swap_info);

        // Check that we have enough assets to send
        if let Some(to_asset) = to_asset {
            let max_balance = get_max_local_rgb_amount(
                to_asset,
                &state.static_state.ldk_data_dir,
                unlocked_state.channel_manager.list_channels().iter(),
            );
            if swap_info.qty_to > max_balance {
                return Err(APIError::InsufficientAssets);
            }
        }

        let (payment_hash, payment_secret) = unlocked_state
            .channel_manager
            .create_inbound_payment(Some(DUST_LIMIT_MSAT), payload.timeout_sec, None)
            .unwrap();
        unlocked_state.add_maker_swap(payment_hash, swap_data);

        let swapstring = SwapString::from_swap_info(&swap_info, payment_hash).to_string();

        let payment_secret = payment_secret.0.as_hex().to_string();
        let payment_hash = payment_hash.0.as_hex().to_string();
        Ok(Json(MakerInitResponse {
            payment_hash,
            payment_secret,
            swapstring,
        }))
    })
    .await
}

pub(crate) async fn taker(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<TakerRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();
        let swapstring = SwapString::from_str(&payload.swapstring)
            .map_err(|e| APIError::InvalidSwapString(payload.swapstring.clone(), e.to_string()))?;

        if get_current_timestamp() > swapstring.swap_info.expiry {
            return Err(APIError::ExpiredSwapOffer);
        }

        // We are selling assets, check if we have enough
        if let Some(from_asset) = swapstring.swap_info.from_asset {
            let max_balance = get_max_local_rgb_amount(
                from_asset,
                &state.static_state.ldk_data_dir,
                unlocked_state.channel_manager.list_channels().iter(),
            );
            if swapstring.swap_info.qty_from > max_balance {
                return Err(APIError::InsufficientAssets);
            }
        }

        let swap_data = SwapData::create_from_swap_info(&swapstring.swap_info);
        unlocked_state.add_taker_swap(swapstring.payment_hash, swap_data);

        Ok(Json(EmptyResponse {}))
    })
    .await
}
