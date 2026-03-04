use super::*;

pub(crate) async fn list_transactions(
    state: Arc<AppState>,
    skip_sync: bool,
) -> Result<Vec<TransactionData>, APIError> {
    let guard = state.check_unlocked().await?;
    let unlocked_state = guard.as_ref().unwrap();

    let mut transactions = vec![];
    for tx in unlocked_state.rgb_list_transactions(skip_sync)? {
        transactions.push(TransactionData {
            transaction_type: match tx.transaction_type {
                rgb_lib::TransactionType::RgbSend => TransactionType::RgbSend,
                rgb_lib::TransactionType::Drain => TransactionType::Drain,
                rgb_lib::TransactionType::CreateUtxos => TransactionType::CreateUtxos,
                rgb_lib::TransactionType::User => TransactionType::User,
            },
            txid: tx.txid,
            received: tx.received,
            sent: tx.sent,
            fee: tx.fee,
            confirmation_time: tx.confirmation_time.map(|ct| BlockTime {
                height: ct.height,
                timestamp: ct.timestamp,
            }),
        });
    }

    Ok(transactions)
}

pub(crate) async fn list_transfers(
    state: Arc<AppState>,
    asset_id: String,
) -> Result<Vec<TransferData>, APIError> {
    let guard = state.check_unlocked().await?;
    let unlocked_state = guard.as_ref().unwrap();

    let mut transfers = vec![];
    for transfer in unlocked_state.rgb_list_transfers(asset_id)? {
        transfers.push(TransferData {
            idx: transfer.idx,
            created_at: transfer.created_at,
            updated_at: transfer.updated_at,
            status: match transfer.status {
                rgb_lib::TransferStatus::WaitingCounterparty => TransferStatus::WaitingCounterparty,
                rgb_lib::TransferStatus::WaitingConfirmations => {
                    TransferStatus::WaitingConfirmations
                }
                rgb_lib::TransferStatus::Settled => TransferStatus::Settled,
                rgb_lib::TransferStatus::Failed => TransferStatus::Failed,
            },
            requested_assignment: transfer.requested_assignment.map(|a| a.into()),
            assignments: transfer.assignments.into_iter().map(|a| a.into()).collect(),
            kind: match transfer.kind {
                rgb_lib::TransferKind::Issuance => TransferKind::Issuance,
                rgb_lib::TransferKind::ReceiveBlind => TransferKind::ReceiveBlind,
                rgb_lib::TransferKind::ReceiveWitness => TransferKind::ReceiveWitness,
                rgb_lib::TransferKind::Send => TransferKind::Send,
                rgb_lib::TransferKind::Inflation => TransferKind::Inflation,
            },
            txid: transfer.txid,
            recipient_id: transfer.recipient_id,
            receive_utxo: transfer.receive_utxo.map(|u| u.to_string()),
            change_utxo: transfer.change_utxo.map(|u| u.to_string()),
            expiration: transfer.expiration,
            transport_endpoints: transfer
                .transport_endpoints
                .iter()
                .map(|tte| TransferTransportEndpointData {
                    endpoint: tte.endpoint.clone(),
                    transport_type: match tte.transport_type {
                        rgb_lib::TransportType::JsonRpc => TransportType::JsonRpc,
                    },
                    used: tte.used,
                })
                .collect(),
        });
    }
    Ok(transfers)
}

pub(crate) async fn list_unspents(
    state: Arc<AppState>,
    skip_sync: bool,
) -> Result<Vec<UnspentData>, APIError> {
    let guard = state.check_unlocked().await?;
    let unlocked_state = guard.as_ref().unwrap();

    let mut unspents = vec![];
    for unspent in unlocked_state.rgb_list_unspents(skip_sync)? {
        unspents.push(UnspentData {
            utxo: UtxoData {
                outpoint: unspent.utxo.outpoint.to_string(),
                btc_amount: unspent.utxo.btc_amount,
                colorable: unspent.utxo.colorable,
            },
            rgb_allocations: unspent
                .rgb_allocations
                .iter()
                .map(|a| RgbAllocationData {
                    asset_id: a.asset_id.clone(),
                    assignment: a.assignment.clone().into(),
                    settled: a.settled,
                })
                .collect(),
        });
    }
    Ok(unspents)
}
