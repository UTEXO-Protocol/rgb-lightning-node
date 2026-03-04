use super::*;

pub(crate) async fn close_channel(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<CloseChannelRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        let channel_id_vec = hex_str_to_vec(&payload.channel_id);
        if channel_id_vec.is_none() || channel_id_vec.as_ref().unwrap().len() != 32 {
            return Err(APIError::InvalidChannelID);
        }
        let requested_cid = ChannelId(channel_id_vec.unwrap().try_into().unwrap());

        let peer_pubkey_vec = match hex_str_to_vec(&payload.peer_pubkey) {
            Some(peer_pubkey_vec) => peer_pubkey_vec,
            None => return Err(APIError::InvalidPubkey),
        };
        let peer_pubkey = match PublicKey::from_slice(&peer_pubkey_vec) {
            Ok(peer_pubkey) => peer_pubkey,
            Err(_) => return Err(APIError::InvalidPubkey),
        };

        if let Some(chan_details) = unlocked_state
            .channel_manager
            .list_channels()
            .iter()
            .find(|c| c.channel_id == requested_cid)
        {
            match chan_details.channel_shutdown_state {
                Some(ChannelShutdownState::NotShuttingDown) => {}
                _ => {
                    return Err(APIError::CannotCloseChannel(s!(
                        "Channel is already being closed"
                    )))
                }
            }
        } else {
            return Err(APIError::UnknownChannelId);
        }

        if payload.force {
            match unlocked_state
                .channel_manager
                .force_close_broadcasting_latest_txn(
                    &requested_cid,
                    &peer_pubkey,
                    "Manually force-closed".to_string(),
                ) {
                Ok(()) => tracing::info!("EVENT: initiating channel force-close"),
                Err(e) => match e {
                    LDKAPIError::APIMisuseError { err } => {
                        return Err(APIError::FailedClosingChannel(err))
                    }
                    _ => return Err(APIError::CannotCloseChannel(format!("{e:?}"))),
                },
            }
        } else {
            match unlocked_state
                .channel_manager
                .close_channel(&requested_cid, &peer_pubkey)
            {
                Ok(()) => tracing::info!("EVENT: initiating channel close"),
                Err(e) => match e {
                    LDKAPIError::APIMisuseError { err } => {
                        return Err(APIError::FailedClosingChannel(err))
                    }
                    _ => return Err(APIError::CannotCloseChannel(format!("{e:?}"))),
                },
            }
        }

        Ok(Json(EmptyResponse {}))
    })
    .await
}

pub(crate) async fn connect_peer(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<ConnectPeerRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        let (peer_pubkey, peer_addr) = parse_peer_info(payload.peer_pubkey_and_addr.to_string())?;

        if let Some(peer_addr) = peer_addr {
            connect_peer_if_necessary(peer_pubkey, peer_addr, unlocked_state.peer_manager.clone())
                .await?;
            disk::persist_channel_peer(
                &state.static_state.ldk_data_dir.join(CHANNEL_PEER_DATA),
                &peer_pubkey,
                &peer_addr,
            )?;
        } else {
            return Err(APIError::InvalidPeerInfo(s!(
                "incorrectly formatted peer info. Should be formatted as: `pubkey@host:port`"
            )));
        }

        Ok(Json(EmptyResponse {}))
    })
    .await
}

pub(crate) async fn disconnect_peer(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<DisconnectPeerRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        let peer_pubkey = match PublicKey::from_str(&payload.peer_pubkey) {
            Ok(pubkey) => pubkey,
            Err(_e) => return Err(APIError::InvalidPubkey),
        };

        //check for open channels with peer
        for channel in unlocked_state.channel_manager.list_channels() {
            if channel.counterparty.node_id == peer_pubkey {
                return Err(APIError::FailedPeerDisconnection(s!(
                    "node has an active channel with this peer, close any channels first"
                )));
            }
        }

        disk::delete_channel_peer(
            &state.static_state.ldk_data_dir.join(CHANNEL_PEER_DATA),
            payload.peer_pubkey,
        )?;

        //check the pubkey matches a valid connected peer
        if unlocked_state
            .peer_manager
            .peer_by_node_id(&peer_pubkey)
            .is_none()
        {
            return Err(APIError::FailedPeerDisconnection(format!(
                "Could not find peer {peer_pubkey}"
            )));
        }

        unlocked_state
            .peer_manager
            .disconnect_by_node_id(peer_pubkey);

        Ok(Json(EmptyResponse {}))
    })
    .await
}

pub(crate) async fn send_onion_message(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<SendOnionMessageRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let guard = state.check_unlocked().await?;
        let unlocked_state = guard.as_ref().unwrap();

        if payload.node_ids.is_empty() {
            return Err(APIError::InvalidNodeIds(s!(
                "sendonionmessage requires at least one node id for the path"
            )));
        }

        let mut intermediate_nodes = Vec::new();
        for pk_str in payload.node_ids {
            let node_pubkey_vec = match hex_str_to_vec(&pk_str) {
                Some(peer_pubkey_vec) => peer_pubkey_vec,
                None => {
                    return Err(APIError::InvalidNodeIds(format!(
                        "Couldn't parse peer_pubkey '{pk_str}'"
                    )))
                }
            };
            let node_pubkey = match PublicKey::from_slice(&node_pubkey_vec) {
                Ok(peer_pubkey) => peer_pubkey,
                Err(_) => {
                    return Err(APIError::InvalidNodeIds(format!(
                        "Couldn't parse peer_pubkey '{pk_str}'"
                    )))
                }
            };
            intermediate_nodes.push(node_pubkey);
        }

        if payload.tlv_type < 64 {
            return Err(APIError::InvalidTlvType(s!(
                "need an integral message type above 64"
            )));
        }

        let data = hex_str_to_vec(&payload.data)
            .ok_or(APIError::InvalidOnionData(s!("need a hex data string")))?;

        let destination = Destination::Node(intermediate_nodes.pop().unwrap());
        let message_send_instructions = MessageSendInstructions::WithoutReplyPath { destination };

        unlocked_state
            .onion_messenger
            .send_onion_message(
                UserOnionMessageContents {
                    tlv_type: payload.tlv_type,
                    data,
                },
                message_send_instructions,
            )
            .map_err(|e| APIError::FailedSendingOnionMessage(format!("{e:?}")))?;

        tracing::info!("SUCCESS: forwarded onion message to first hop");

        Ok(Json(EmptyResponse {}))
    })
    .await
}
