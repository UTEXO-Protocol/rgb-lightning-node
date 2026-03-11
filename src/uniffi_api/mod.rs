pub(crate) mod state;
mod types;

use std::str::FromStr;

use crate::sdk;
use crate::{NodeConfig, NodeHandle};

use state::{
    block_on_app, block_on_sdk, clear_uniffi_node_handle, get_uniffi_app_state,
    is_uniffi_app_state_initialized, set_uniffi_node_handle,
};
pub(crate) use state::{clear_uniffi_app_state, set_uniffi_app_state};
pub use types::*;

pub fn uniffi_healthcheck() -> String {
    "rgb_lightning_node_uniffi_ready".to_string()
}

pub fn uniffi_is_initialized() -> bool {
    is_uniffi_app_state_initialized()
}

fn network_from_str(network: &str) -> Result<rgb_lib::BitcoinNetwork, RlnError> {
    match network.to_lowercase().as_str() {
        "mainnet" => Ok(rgb_lib::BitcoinNetwork::Mainnet),
        "testnet" => Ok(rgb_lib::BitcoinNetwork::Testnet),
        "testnet4" => Ok(rgb_lib::BitcoinNetwork::Testnet4),
        "signet" => Ok(rgb_lib::BitcoinNetwork::Signet),
        "regtest" => Ok(rgb_lib::BitcoinNetwork::Regtest),
        _ => Err(RlnError::InvalidRequest),
    }
}

fn handle_from_request(request: SdkInitRequestV1) -> Result<NodeHandle, RlnError> {
    let network = network_from_str(&request.network)?;
    let config = NodeConfig {
        storage_dir_path: std::path::PathBuf::from(request.storage_dir_path),
        daemon_listening_port: request.daemon_listening_port,
        ldk_peer_listening_port: request.ldk_peer_listening_port,
        network,
        max_media_upload_size_mb: request.max_media_upload_size_mb,
        root_public_key: None,
    };
    block_on_app(NodeHandle::new(config))
}

fn send_rgb_from_state(
    state: std::sync::Arc<crate::utils::AppState>,
    request: SendRgbRequestV1,
) -> Result<SendRgbResponseV1, RlnError> {
    if request.recipient_groups.is_empty() {
        return Err(RlnError::InvalidRequest);
    }
    let recipient_map = request
        .recipient_groups
        .into_iter()
        .map(|group| {
            let asset_id = group.asset_id.to_string();
            let recipients = group
                .recipients
                .into_iter()
                .map(|r| {
                    let assignment = match (r.assignment_kind, r.assignment_amount) {
                        (AssignmentKindV1::Fungible, Some(v)) => crate::routes::Assignment::Fungible(v),
                        (AssignmentKindV1::InflationRight, Some(v)) => {
                            crate::routes::Assignment::InflationRight(v)
                        }
                        (AssignmentKindV1::NonFungible, None) => crate::routes::Assignment::NonFungible,
                        (AssignmentKindV1::ReplaceRight, None) => crate::routes::Assignment::ReplaceRight,
                        (AssignmentKindV1::Any, None) => crate::routes::Assignment::Any,
                        _ => return Err(RlnError::InvalidRequest),
                    };
                    let recipient = crate::routes::Recipient {
                        recipient_id: r.recipient_id.0,
                        witness_data: r.witness_data.map(|w| crate::routes::WitnessData {
                            amount_sat: w.amount_sat,
                            blinding: w.blinding,
                        }),
                        assignment,
                        transport_endpoints: r.transport_endpoints.into_iter().map(|e| e.0).collect(),
                    };
                    Ok::<rgb_lib::wallet::Recipient, RlnError>(recipient.into())
                })
                .collect::<Result<Vec<_>, RlnError>>()?;
            Ok((asset_id, recipients))
        })
        .collect::<Result<std::collections::HashMap<_, _>, RlnError>>()?;

    let data = block_on_sdk(sdk::send_rgb(
        state,
        recipient_map,
        request.donation,
        request.fee_rate,
        request.min_confirmations,
        request.skip_sync,
    ))?;
    let txid = Txid::from_str(&data.txid).map_err(|_| RlnError::Internal)?;
    Ok(SendRgbResponseV1 { txid, batch_transfer_idx: data.batch_transfer_idx })
}

impl SdkNodeV1 {
    pub fn create(request: SdkInitRequestV1) -> Result<Self, RlnError> {
        let handle = handle_from_request(request)?;
        Ok(Self { handle })
    }

    pub fn shutdown(&self) {
        let handle = self.handle.clone();
        let _ = block_on_sdk(async move {
            handle.shutdown().await;
            Ok::<(), crate::error::APIError>(())
        });
    }

    pub fn node_info(&self) -> Result<NodeInfoV1, RlnError> {
        let state = self.handle.app_state();
        let data = block_on_sdk(sdk::node_info(state))?;
        let pubkey =
            bitcoin::secp256k1::PublicKey::from_str(&data.pubkey).map_err(|_| RlnError::Internal)?;
        Ok(NodeInfoV1 {
            pubkey,
            num_channels: data.num_channels as u64,
            num_peers: data.num_peers as u64,
            network_nodes: data.network_nodes as u64,
            network_channels: data.network_channels as u64,
        })
    }

    pub fn get_channel_id(&self, temporary_channel_id: ChannelId) -> Result<ChannelId, RlnError> {
        use bitcoin::hex::DisplayHex;
        use bitcoin::hex::FromHex;

        let state = self.handle.app_state();
        let data =
            block_on_sdk(sdk::get_channel_id(state, temporary_channel_id.0.as_hex().to_string()))?;
        let bytes = Vec::<u8>::from_hex(&data.channel_id).map_err(|_| RlnError::Internal)?;
        if bytes.len() != 32 {
            return Err(RlnError::Internal);
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(lightning::ln::types::ChannelId(arr))
    }

    pub fn get_payment(&self, payment_hash: PaymentHash) -> Result<PaymentV1, RlnError> {
        use bitcoin::hex::DisplayHex;

        let state = self.handle.app_state();
        let data = block_on_sdk(sdk::get_payment(state, payment_hash.0.as_hex().to_string()))?;
        let payee_pubkey =
            PublicKey::from_str(&data.payee_pubkey).map_err(|_| RlnError::Internal)?;
        let asset_id = match data.asset_id {
            Some(asset_id) => Some(ContractId::from_str(&asset_id).map_err(|_| RlnError::Internal)?),
            None => None,
        };
        let payment_hash = <PaymentHash as UniffiCustomTypeConverter>::into_custom(data.payment_hash)
            .map_err(|_| RlnError::Internal)?;
        let status = match data.status {
            crate::sdk::HtlcStatus::Pending => HtlcStatusV1::Pending,
            crate::sdk::HtlcStatus::Succeeded => HtlcStatusV1::Succeeded,
            crate::sdk::HtlcStatus::Failed => HtlcStatusV1::Failed,
        };

        Ok(PaymentV1 {
            amt_msat: data.amt_msat,
            asset_amount: data.asset_amount,
            asset_id,
            payment_hash,
            inbound: data.inbound,
            status,
            created_at: data.created_at,
            updated_at: data.updated_at,
            payee_pubkey,
        })
    }

    pub fn get_swap(&self, payment_hash: PaymentHash, taker: bool) -> Result<SwapV1, RlnError> {
        use bitcoin::hex::DisplayHex;

        let state = self.handle.app_state();
        let data = block_on_sdk(sdk::get_swap(state, payment_hash.0.as_hex().to_string(), taker))?;
        let from_asset = match data.from_asset {
            Some(asset_id) => Some(ContractId::from_str(&asset_id).map_err(|_| RlnError::Internal)?),
            None => None,
        };
        let to_asset = match data.to_asset {
            Some(asset_id) => Some(ContractId::from_str(&asset_id).map_err(|_| RlnError::Internal)?),
            None => None,
        };
        let payment_hash = <PaymentHash as UniffiCustomTypeConverter>::into_custom(data.payment_hash)
            .map_err(|_| RlnError::Internal)?;
        let status = match data.status {
            crate::sdk::SwapStatus::Waiting => SwapStatusV1::Waiting,
            crate::sdk::SwapStatus::Pending => SwapStatusV1::Pending,
            crate::sdk::SwapStatus::Succeeded => SwapStatusV1::Succeeded,
            crate::sdk::SwapStatus::Expired => SwapStatusV1::Expired,
            crate::sdk::SwapStatus::Failed => SwapStatusV1::Failed,
        };

        Ok(SwapV1 {
            qty_from: data.qty_from,
            qty_to: data.qty_to,
            from_asset,
            to_asset,
            payment_hash,
            status,
            requested_at: data.requested_at,
            initiated_at: data.initiated_at,
            expires_at: data.expires_at,
            completed_at: data.completed_at,
        })
    }

    pub fn ln_invoice(&self, request: LnInvoiceRequestV1) -> Result<LnInvoiceResponseV1, RlnError> {
        let state = self.handle.app_state();
        let asset_id = request.asset_id.map(|a| a.to_string());
        let data = block_on_sdk(sdk::create_ln_invoice(
            state,
            request.amt_msat,
            request.expiry_sec,
            asset_id,
            request.asset_amount,
            crate::routes::INVOICE_MIN_MSAT,
        ))?;
        let invoice = Bolt11Invoice::from_str(&data.invoice).map_err(|_| RlnError::Internal)?;
        Ok(LnInvoiceResponseV1 { invoice })
    }

    pub fn send_rgb(&self, request: SendRgbRequestV1) -> Result<SendRgbResponseV1, RlnError> {
        send_rgb_from_state(self.handle.app_state(), request)
    }
}

pub fn sdk_initialize(request: SdkInitRequestV1) -> Result<(), RlnError> {
    // Compatibility path for existing clients using process-global state.
    let handle = handle_from_request(request)?;
    set_uniffi_node_handle(handle);
    Ok(())
}

pub fn sdk_shutdown() {
    if let Ok(state) = get_uniffi_app_state() {
        let _ = block_on_sdk(async move {
            let handle = NodeHandle::from_app_state(state);
            handle.shutdown().await;
            Ok::<(), crate::error::APIError>(())
        });
    }
    clear_uniffi_node_handle();
}

pub fn sdk_node_info() -> Result<NodeInfoV1, RlnError> {
    let handle = NodeHandle::from_app_state(get_uniffi_app_state()?);
    SdkNodeV1 { handle }.node_info()
}

pub fn sdk_get_channel_id(temporary_channel_id: ChannelId) -> Result<ChannelId, RlnError> {
    let handle = NodeHandle::from_app_state(get_uniffi_app_state()?);
    SdkNodeV1 { handle }.get_channel_id(temporary_channel_id)
}

pub fn sdk_get_payment(payment_hash: PaymentHash) -> Result<PaymentV1, RlnError> {
    let handle = NodeHandle::from_app_state(get_uniffi_app_state()?);
    SdkNodeV1 { handle }.get_payment(payment_hash)
}

pub fn sdk_get_swap(payment_hash: PaymentHash, taker: bool) -> Result<SwapV1, RlnError> {
    let handle = NodeHandle::from_app_state(get_uniffi_app_state()?);
    SdkNodeV1 { handle }.get_swap(payment_hash, taker)
}

pub fn sdk_ln_invoice(request: LnInvoiceRequestV1) -> Result<LnInvoiceResponseV1, RlnError> {
    let handle = NodeHandle::from_app_state(get_uniffi_app_state()?);
    SdkNodeV1 { handle }.ln_invoice(request)
}

pub fn sdk_send_rgb(request: SendRgbRequestV1) -> Result<SendRgbResponseV1, RlnError> {
    let handle = NodeHandle::from_app_state(get_uniffi_app_state()?);
    SdkNodeV1 { handle }.send_rgb(request)
}

uniffi::include_scaffolding!("rgb_lightning_node");

#[cfg(test)]
mod tests;
