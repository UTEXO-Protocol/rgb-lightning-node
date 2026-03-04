use super::*;

pub(crate) async fn open_channel(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<OpenChannelRequest>, APIError>,
) -> Result<Json<OpenChannelResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        if *unlocked_state.rgb_send_lock.lock().unwrap() {
            return Err(APIError::OpenChannelInProgress);
        }

        let temporary_channel_id = if let Some(tmp_chan_id_str) = payload.temporary_channel_id {
            let tmp_chan_id = check_channel_id(&tmp_chan_id_str)?;
            if unlocked_state.channel_ids().contains_key(&tmp_chan_id) {
                return Err(APIError::TemporaryChannelIdAlreadyUsed);
            }
            Some(tmp_chan_id)
        } else {
            None
        };

        let colored_info = match (payload.asset_id, payload.asset_amount) {
            (Some(_), Some(amt)) if amt < OPENCHANNEL_MIN_RGB_AMT => {
                return Err(APIError::InvalidAmount(format!(
                    "Channel RGB amount must be equal to or higher than {OPENCHANNEL_MIN_RGB_AMT}"
                )));
            }
            (Some(asset), Some(amt)) => {
                let asset =
                    ContractId::from_str(&asset).map_err(|_| APIError::InvalidAssetID(asset))?;
                Some((asset, amt))
            }
            (None, None) => None,
            _ => {
                return Err(APIError::IncompleteRGBInfo);
            }
        };

        if colored_info.is_some() && payload.capacity_sat < OPENRGBCHANNEL_MIN_SAT {
            return Err(APIError::InvalidAmount(format!(
                "RGB channel amount must be equal to or higher than {OPENRGBCHANNEL_MIN_SAT} sats"
            )));
        } else if payload.capacity_sat < OPENCHANNEL_MIN_SAT {
            return Err(APIError::InvalidAmount(format!(
                "Channel amount must be equal to or higher than {OPENCHANNEL_MIN_SAT} sats"
            )));
        }
        if payload.capacity_sat > OPENCHANNEL_MAX_SAT {
            return Err(APIError::InvalidAmount(format!(
                "Channel amount must be equal to or less than {OPENCHANNEL_MAX_SAT} sats"
            )));
        }

        if payload.push_msat > payload.capacity_sat * 1000 {
            return Err(APIError::InvalidAmount(s!(
                "Channel push amount cannot be higher than the capacity"
            )));
        }

        if colored_info.is_some() && !payload.with_anchors {
            return Err(APIError::AnchorsRequired);
        }

        let (peer_pubkey, mut peer_addr) =
            parse_peer_info(payload.peer_pubkey_and_opt_addr.to_string())?;

        let peer_data_path = state.static_state.ldk_data_dir.join(CHANNEL_PEER_DATA);
        if peer_addr.is_none() {
            if let Some(peer) = unlocked_state.peer_manager.peer_by_node_id(&peer_pubkey) {
                if let Some(socket_address) = peer.socket_address {
                    if let Ok(mut socket_addrs) = socket_address.to_socket_addrs() {
                        // assuming there's only one IP address
                        peer_addr = socket_addrs.next();
                    }
                }
            }
        }
        if peer_addr.is_none() {
            let peer_info = disk::read_channel_peer_data(&peer_data_path)?;
            for (pubkey, addr) in peer_info.into_iter() {
                if pubkey == peer_pubkey {
                    peer_addr = Some(addr);
                    break;
                }
            }
        }
        if let Some(peer_addr) = peer_addr {
            connect_peer_if_necessary(peer_pubkey, peer_addr, unlocked_state.peer_manager.clone())
                .await?;
            disk::persist_channel_peer(&peer_data_path, &peer_pubkey, &peer_addr)?;
        } else {
            return Err(APIError::InvalidPeerInfo(s!(
                "cannot find the address for the provided pubkey"
            )));
        }

        let mut channel_config = ChannelConfig::default();
        if let Some(fee_base_msat) = payload.fee_base_msat {
            channel_config.forwarding_fee_base_msat = fee_base_msat;
        }
        if let Some(fee_proportional_millionths) = payload.fee_proportional_millionths {
            channel_config.forwarding_fee_proportional_millionths = fee_proportional_millionths;
        }
        let config = UserConfig {
            channel_handshake_limits: ChannelHandshakeLimits {
                // lnd's max to_self_delay is 2016, so we want to be compatible.
                their_to_self_delay: 2016,
                ..Default::default()
            },
            channel_handshake_config: ChannelHandshakeConfig {
                announce_for_forwarding: payload.public,
                our_htlc_minimum_msat: HTLC_MIN_MSAT,
                minimum_depth: MIN_CHANNEL_CONFIRMATIONS as u32,
                negotiate_anchors_zero_fee_htlc_tx: payload.with_anchors,
                ..Default::default()
            },
            channel_config,
            ..Default::default()
        };

        let consignment_endpoint = if let Some((contract_id, asset_amount)) = &colored_info {
            let balance = unlocked_state.rgb_get_asset_balance(*contract_id)?;
            let spendable_rgb_amount = balance.spendable;

            if *asset_amount > spendable_rgb_amount {
                return Err(APIError::InsufficientAssets);
            }

            Some(RgbTransport::from_str(&unlocked_state.proxy_endpoint).unwrap())
        } else {
            None
        };

        let schema = if let Some((contract_id, asset_amount)) = &colored_info {
            let mut fake_p2wsh: [u8; 34] = [0; 34];
            fake_p2wsh[1] = 32;
            let script_buf = ScriptBuf::from_bytes(fake_p2wsh.to_vec());
            let recipient_id = recipient_id_from_script_buf(script_buf, state.static_state.network);
            let asset_id = contract_id.to_string();
            let schema = unlocked_state
                .rgb_get_asset_metadata(*contract_id)?
                .asset_schema;
            let assignment = match schema {
                RgbLibAssetSchema::Nia | RgbLibAssetSchema::Cfa => {
                    Assignment::Fungible(*asset_amount)
                }
                RgbLibAssetSchema::Uda => Assignment::NonFungible,
                RgbLibAssetSchema::Ifa => todo!(),
            };

            let recipient_map = map! {
                asset_id => vec![RgbLibRecipient {
                    recipient_id,
                    witness_data: Some(RgbLibWitnessData {
                        amount_sat: payload.capacity_sat,
                        blinding: Some(STATIC_BLINDING + 1),
                    }),
                    assignment: assignment.into(),
                    transport_endpoints: vec![unlocked_state.proxy_endpoint.clone()]
            }]};

            let unlocked_state_copy = unlocked_state.clone();
            tokio::task::spawn_blocking(move || {
                unlocked_state_copy.rgb_send_begin(
                    recipient_map,
                    true,
                    FEE_RATE,
                    MIN_CHANNEL_CONFIRMATIONS,
                )
            })
            .await
            .unwrap()?;
            Some(schema)
        } else {
            None
        };

        *unlocked_state.rgb_send_lock.lock().unwrap() = true;
        tracing::debug!("RGB send lock set to true");

        let temporary_channel_id = unlocked_state
            .channel_manager
            .create_channel(
                peer_pubkey,
                payload.capacity_sat,
                payload.push_msat,
                0,
                temporary_channel_id,
                Some(config),
                consignment_endpoint,
            )
            .map_err(|e| {
                *unlocked_state.rgb_send_lock.lock().unwrap() = false;
                tracing::debug!("RGB send lock set to false (open channel failure: {e:?})");
                match e {
                    LDKAPIError::APIMisuseError { err }
                        if err.contains("fee for initial commitment transaction") =>
                    {
                        let mut commitment_tx_fee = 0;
                        let re =
                            Regex::new(r"fee for initial commitment transaction fee of (\d+).")
                                .unwrap();
                        if let Some(captures) = re.captures(&err) {
                            if let Some(fee_str) = captures.get(1) {
                                commitment_tx_fee = fee_str.as_str().parse().unwrap();
                            }
                        }
                        APIError::InsufficientCapacity(commitment_tx_fee)
                    }
                    _ => APIError::FailedOpenChannel(format!("{e:?}")),
                }
            })?;
        let temporary_channel_id = temporary_channel_id.0.as_hex().to_string();
        tracing::info!("EVENT: initiated channel with peer {}", peer_pubkey);

        if let Some((contract_id, asset_amount)) = &colored_info {
            let rgb_info = RgbInfo {
                contract_id: *contract_id,
                schema: schema.unwrap(),
                local_rgb_amount: *asset_amount,
                remote_rgb_amount: 0,
            };
            write_rgb_channel_info(
                &get_rgb_channel_info_path(
                    &temporary_channel_id,
                    &state.static_state.ldk_data_dir,
                    true,
                ),
                &rgb_info,
            );
            write_rgb_channel_info(
                &get_rgb_channel_info_path(
                    &temporary_channel_id,
                    &state.static_state.ldk_data_dir,
                    false,
                ),
                &rgb_info,
            );
        }

        Ok(Json(OpenChannelResponse {
            temporary_channel_id,
        }))
    })
    .await
}
