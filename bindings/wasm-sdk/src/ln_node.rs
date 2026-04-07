use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::str::FromStr;
use std::time::Duration;

use bitcoin_hashes::sha256::Hash as Sha256;
use bitcoin_hashes::Hash as _;
use lightning_invoice::Bolt11Invoice;
use lightning_invoice::Currency;
use lightning_invoice::InvoiceBuilder;
use lightning_invoice::PaymentSecret;
use secp256k1::{Message as SecpMessage, PublicKey as SecpPublicKey, Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use crate::ldk_event_applier::{
    ensure_manual_event_ingestion_allowed, ensure_manual_status_update_allowed,
};
use crate::ldk_runtime::{
    ldk_runtime_manager, LdkRuntimeChannelStateData, LdkRuntimeManager, LdkRuntimePaymentStateData,
    LdkRuntimePeerStateData, LdkRuntimeStatusData,
};
use crate::peer_session::{
    clear_rln_ldk_peer_manager_hooks, install_rln_ldk_peer_manager_hooks, RlnLdkPeerManagerHooks,
    RlnWasmPeerSession, RlnWasmRustPeerManagerBridge,
};
use crate::runtime_store::{browser_persistent_state_store, RuntimeStateStore};

const SDK_HTLC_MIN_MSAT: u64 = 3_000_000;
const SDK_INVOICE_MIN_MSAT: u64 = SDK_HTLC_MIN_MSAT;
const SDK_OPENRGBCHANNEL_MIN_SAT: u64 = SDK_HTLC_MIN_MSAT / 1000 * 10 + 10;
const SDK_OPENCHANNEL_MIN_SAT: u64 = 5_506;
const SDK_OPENCHANNEL_MAX_SAT: u64 = 16_777_215;
const SDK_OPENCHANNEL_MIN_RGB_AMT: u64 = 1;

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodePeerData {
    pub pubkey: String,
    pub peer_addr: String,
    pub started: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodeInfoData {
    pub runtime: String,
    pub ldk_over_websocket: bool,
    pub num_peers: usize,
    pub num_channels: usize,
    pub num_usable_channels: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodeNetworkInfoData {
    pub network: String,
    pub height: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodeChannelData {
    pub temporary_channel_id: String,
    pub channel_id: String,
    pub peer_pubkey: String,
    pub status: String,
    pub ready: bool,
    pub is_usable: bool,
    pub public: bool,
    pub capacity_sat: u64,
    pub asset_id: Option<String>,
    pub asset_local_amount: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodePaymentData {
    pub amt_msat: Option<u64>,
    pub asset_amount: Option<u64>,
    pub asset_id: Option<String>,
    pub payment_hash: String,
    pub inbound: bool,
    pub status: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub payee_pubkey: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodeSendPaymentResult {
    pub payment_id: String,
    pub payment_hash: Option<String>,
    pub payment_secret: Option<String>,
    pub status: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodeKeysendResult {
    pub payment_hash: String,
    pub payment_preimage: String,
    pub status: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodeDecodeLnInvoiceData {
    pub amt_msat: Option<u64>,
    pub expiry_sec: u64,
    pub timestamp: u64,
    pub asset_id: Option<String>,
    pub asset_amount: Option<u64>,
    pub payment_hash: String,
    pub payment_secret: String,
    pub payee_pubkey: Option<String>,
    pub network: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodeInvoiceStatusData {
    pub status: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodeSignMessageData {
    pub signed_message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RlnWasmNodeCreateLnInvoiceData {
    pub invoice: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RlnWasmNodeRuntimeEventData {
    pub seq: u64,
    pub source: String,
    pub event_kind: String,
    pub payload_hex: String,
    pub payment_hash: Option<String>,
    pub status: Option<String>,
    pub applied: bool,
    pub error: Option<String>,
    pub received_at: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct PaymentStatusEvent {
    payment_hash: String,
    status: String,
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
enum RuntimeEventApplyMode {
    StrictPaymentStatus,
    TolerantTransport,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "event")]
enum RuntimeTransportEvent {
    #[serde(rename = "peer_disconnected")]
    PeerDisconnected { peer_pubkey: String },
    #[serde(rename = "peer_reconnected")]
    PeerReconnected { peer_pubkey: String },
    #[serde(rename = "channel_closed")]
    ChannelClosed { channel_id: String },
    #[serde(rename = "channel_usable")]
    ChannelUsable { channel_id: String },
    #[serde(rename = "channel_unusable")]
    ChannelUnusable { channel_id: String },
}

impl RuntimeTransportEvent {
    fn event_kind(&self) -> &'static str {
        match self {
            Self::PeerDisconnected { .. } => "peer_disconnected",
            Self::PeerReconnected { .. } => "peer_reconnected",
            Self::ChannelClosed { .. } => "channel_closed",
            Self::ChannelUsable { .. } => "channel_usable",
            Self::ChannelUnusable { .. } => "channel_unusable",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct RuntimeTransportEventApplyData {
    event_kind: String,
    applied: bool,
}

struct PeerEntry {
    peer_addr: String,
    session: Rc<RlnWasmPeerSession>,
}

struct ChannelEntry {
    temporary_channel_id: String,
    data: RlnWasmNodeChannelData,
}

struct PaymentEntry {
    data: RlnWasmNodePaymentData,
}

enum PendingPeerHookEvent {
    Payload(String),
    SocketDisconnected,
    Error(String),
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct RuntimeEventLogSnapshot {
    events: Vec<RlnWasmNodeRuntimeEventData>,
    next_seq: u64,
}

thread_local! {
    static RUNTIME_EVENT_LOG_STORAGE: RefCell<HashMap<String, RuntimeEventLogSnapshot>> =
        RefCell::new(HashMap::new());
}

const RUNTIME_EVENT_LOG_STORAGE_PREFIX: &str = "rln:wasm:runtime-events:";

#[cfg(test)]
pub(crate) fn reset_runtime_event_log_storage_for_tests() {
    RUNTIME_EVENT_LOG_STORAGE.with(|state| {
        state.borrow_mut().clear();
    });
}

#[wasm_bindgen]
pub struct RlnWasmNode {
    proxy_url: String,
    runtime_event_store_key: String,
    bridge: RlnWasmRustPeerManagerBridge,
    ldk_runtime: Rc<dyn LdkRuntimeManager>,
    peers: Rc<RefCell<HashMap<String, PeerEntry>>>,
    channels: Rc<RefCell<HashMap<String, ChannelEntry>>>,
    payments: Rc<RefCell<HashMap<String, PaymentEntry>>>,
    pending_peer_hook_events: Rc<RefCell<Vec<PendingPeerHookEvent>>>,
    runtime_events: Rc<RefCell<Vec<RlnWasmNodeRuntimeEventData>>>,
    next_channel_seq: RefCell<u64>,
    next_payment_seq: RefCell<u64>,
    next_runtime_event_seq: Rc<RefCell<u64>>,
    network: RefCell<String>,
}

#[wasm_bindgen]
impl RlnWasmNode {
    fn ensure_runtime_ready(&self) -> Result<(), JsValue> {
        crate::ensure_sdk_node_runtime_allowed()?;
        self.ldk_runtime.ensure_started()
    }

    #[wasm_bindgen(constructor)]
    pub fn new(proxy_url: String) -> Result<RlnWasmNode, JsValue> {
        Self::new_with_runtime_backend(proxy_url, "scaffold".to_string())
    }

    #[wasm_bindgen(js_name = newWithRuntimeBackend)]
    pub fn new_with_runtime_backend(
        proxy_url: String,
        runtime_backend: String,
    ) -> Result<RlnWasmNode, JsValue> {
        if proxy_url.trim().is_empty() {
            return Err(JsValue::from_str("proxy_url cannot be empty"));
        }
        let runtime_event_store_key = runtime_event_store_key(&proxy_url);
        let runtime_event_snapshot = load_runtime_event_log_snapshot(&runtime_event_store_key);
        let runtime_events = runtime_event_snapshot
            .as_ref()
            .map(|snapshot| snapshot.events.clone())
            .unwrap_or_default();
        let next_runtime_event_seq = runtime_event_snapshot
            .as_ref()
            .map(|snapshot| snapshot.next_seq)
            .unwrap_or(0);
        let runtime_key = format!("node-runtime:{proxy_url}");
        let ldk_runtime = ldk_runtime_manager(runtime_key, Some(runtime_backend))?;
        Ok(Self {
            ldk_runtime,
            proxy_url,
            runtime_event_store_key,
            bridge: RlnWasmRustPeerManagerBridge::new(None)?,
            peers: Rc::new(RefCell::new(HashMap::new())),
            channels: Rc::new(RefCell::new(HashMap::new())),
            payments: Rc::new(RefCell::new(HashMap::new())),
            pending_peer_hook_events: Rc::new(RefCell::new(Vec::new())),
            runtime_events: Rc::new(RefCell::new(runtime_events)),
            next_channel_seq: RefCell::new(0),
            next_payment_seq: RefCell::new(0),
            next_runtime_event_seq: Rc::new(RefCell::new(next_runtime_event_seq)),
            network: RefCell::new("regtest".to_string()),
        })
    }

    #[wasm_bindgen(js_name = connectPeer)]
    pub async fn connect_peer(
        &self,
        peer_addr: String,
        peer_pubkey: String,
    ) -> Result<(), JsValue> {
        self.ensure_runtime_ready()?;
        let peer_pubkey = peer_pubkey.trim().to_string();
        let peer_addr = peer_addr.trim().to_string();
        if peer_pubkey.trim().is_empty() {
            return Err(JsValue::from_str("peer_pubkey cannot be empty"));
        }
        if peer_addr.trim().is_empty() {
            return Err(JsValue::from_str("peer_addr cannot be empty"));
        }
        validate_peer_addr_format(&peer_addr)?;
        if SecpPublicKey::from_str(peer_pubkey.trim()).is_err() {
            return Err(JsValue::from_str("invalid peer_pubkey"));
        }

        if self.use_runtime_state_for_ln_views() {
            let runtime_has_peer = self.ldk_runtime.has_peer(&peer_pubkey);
            let local_has_session = self.peers.borrow().contains_key(&peer_pubkey);
            if runtime_has_peer && local_has_session {
                let _ = self.ldk_runtime.set_peer_started(&peer_pubkey, true);
                return Ok(());
            }
        } else if self.peers.borrow().contains_key(&peer_pubkey) {
            return Ok(());
        }

        let session = self
            .bridge
            .connect_session(
                self.proxy_url.clone(),
                peer_addr.clone(),
                peer_pubkey.clone(),
            )
            .await?;
        session.start().await?;

        self.peers.borrow_mut().insert(
            peer_pubkey.clone(),
            PeerEntry {
                peer_addr,
                session: Rc::new(session),
            },
        );
        if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
                pubkey: peer_pubkey.clone(),
                peer_addr: self
                    .peers
                    .borrow()
                    .get(&peer_pubkey)
                    .map(|entry| entry.peer_addr.clone())
                    .unwrap_or_default(),
                started: true,
            });
        }
        let applied = self
            .apply_and_record_transport_event(
                RuntimeTransportEvent::PeerReconnected { peer_pubkey },
                "node_api",
            )?
            .applied;
        if !applied {
            return Err(JsValue::from_str(
                "failed to apply peer_reconnected transport event",
            ));
        }
        self.persist_runtime_event_log_state();
        Ok(())
    }

    #[wasm_bindgen(js_name = disconnectPeer)]
    pub async fn disconnect_peer(&self, peer_pubkey: String) -> Result<(), JsValue> {
        self.ensure_runtime_ready()?;
        let peer_pubkey = peer_pubkey.trim().to_string();
        if peer_pubkey.trim().is_empty() {
            return Err(JsValue::from_str("peer_pubkey cannot be empty"));
        }
        if SecpPublicKey::from_str(peer_pubkey.trim()).is_err() {
            return Err(JsValue::from_str("invalid peer_pubkey"));
        }

        let session = self
            .peers
            .borrow()
            .get(&peer_pubkey)
            .map(|entry| Rc::clone(&entry.session));
        if let Some(session) = session {
            session.close().await?;
        } else if !(self.use_runtime_state_for_ln_views()
            && self.ldk_runtime.has_peer(&peer_pubkey))
        {
            return Err(JsValue::from_str("peer is not connected"));
        }

        let applied = self
            .apply_and_record_transport_event(
                RuntimeTransportEvent::PeerDisconnected {
                    peer_pubkey: peer_pubkey.clone(),
                },
                "node_api",
            )?
            .applied;
        if !applied {
            return Err(JsValue::from_str("peer is not connected"));
        }
        self.peers.borrow_mut().remove(&peer_pubkey);
        self.persist_runtime_event_log_state();
        Ok(())
    }

    #[wasm_bindgen(js_name = listPeersValue)]
    pub fn list_peers_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let mut data = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .list_peers()
                .into_iter()
                .map(|peer| RlnWasmNodePeerData {
                    pubkey: peer.pubkey,
                    peer_addr: peer.peer_addr,
                    started: peer.started,
                })
                .collect::<Vec<_>>()
        } else {
            self.peers
                .borrow()
                .iter()
                .map(|(pubkey, entry)| RlnWasmNodePeerData {
                    pubkey: pubkey.clone(),
                    peer_addr: entry.peer_addr.clone(),
                    started: entry.session.is_started(),
                })
                .collect::<Vec<_>>()
        };
        data.sort_by(|a, b| a.pubkey.cmp(&b.pubkey));
        crate::js_obj(&data)
    }

    #[wasm_bindgen(js_name = listPeersJson)]
    pub fn list_peers_json(&self) -> Result<String, JsValue> {
        let value = self.list_peers_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = listChannelsValue)]
    pub fn list_channels_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let mut data = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .list_channels()
                .into_iter()
                .map(Self::channel_data_from_runtime_state)
                .collect::<Vec<_>>()
        } else {
            self.channels
                .borrow()
                .values()
                .map(|entry| entry.data.clone())
                .collect::<Vec<_>>()
        };
        data.sort_by(|a, b| a.channel_id.cmp(&b.channel_id));
        crate::js_obj(&data)
    }

    #[wasm_bindgen(js_name = listChannelsJson)]
    pub fn list_channels_json(&self) -> Result<String, JsValue> {
        let value = self.list_channels_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = nodeInfoValue)]
    pub fn node_info_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let (num_channels, num_usable_channels) = if self.use_runtime_state_for_ln_views() {
            let channels = self.ldk_runtime.list_channels();
            let num_channels = channels.len();
            let num_usable_channels = channels.iter().filter(|entry| entry.is_usable).count();
            (num_channels, num_usable_channels)
        } else {
            let channels = self.channels.borrow();
            let num_channels = channels.len();
            let num_usable_channels = channels
                .values()
                .filter(|entry| entry.data.is_usable)
                .count();
            (num_channels, num_usable_channels)
        };
        let runtime_status = self.ldk_runtime.status();
        let data = RlnWasmNodeInfoData {
            runtime: format!(
                "wasm32-unknown-unknown/{}:{}",
                runtime_status.backend, runtime_status.lifecycle_state
            ),
            ldk_over_websocket: true,
            num_peers: if self.use_runtime_state_for_ln_views() {
                self.ldk_runtime.list_peers().len()
            } else {
                self.peers.borrow().len()
            },
            num_channels,
            num_usable_channels,
        };
        crate::js_obj(&data)
    }

    #[wasm_bindgen(js_name = nodeInfoJson)]
    pub fn node_info_json(&self) -> Result<String, JsValue> {
        let value = self.node_info_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = networkInfoValue)]
    pub fn network_info_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        crate::js_obj(&RlnWasmNodeNetworkInfoData {
            network: self.network.borrow().clone(),
            height: 0,
        })
    }

    #[wasm_bindgen(js_name = ldkRuntimeStatusValue)]
    pub fn ldk_runtime_status_value(&self) -> Result<JsValue, JsValue> {
        let status: LdkRuntimeStatusData = self.ldk_runtime.status();
        crate::js_obj(&status)
    }

    #[wasm_bindgen(js_name = ldkRuntimeStatusJson)]
    pub fn ldk_runtime_status_json(&self) -> Result<String, JsValue> {
        let value = self.ldk_runtime_status_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = networkInfoJson)]
    pub fn network_info_json(&self) -> Result<String, JsValue> {
        let value = self.network_info_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = signMessageValue)]
    pub fn sign_message_value(&self, message: String) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let signed_message = self.sign_scaffold_message(message.trim())?;
        crate::js_obj(&RlnWasmNodeSignMessageData { signed_message })
    }

    #[wasm_bindgen(js_name = signMessageJson)]
    pub fn sign_message_json(&self, message: String) -> Result<String, JsValue> {
        let value = self.sign_message_value(message)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = closeAllPeers)]
    pub async fn close_all_peers(&self) -> Result<(), JsValue> {
        self.ensure_runtime_ready()?;
        let peer_pubkeys: Vec<String> = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .list_peers()
                .into_iter()
                .map(|peer| peer.pubkey)
                .collect()
        } else {
            self.peers.borrow().keys().cloned().collect()
        };
        for pubkey in peer_pubkeys {
            self.disconnect_peer(pubkey).await?;
        }
        self.ldk_runtime.stop()?;
        Ok(())
    }

    #[wasm_bindgen(js_name = installAutoPeerManagerHooks)]
    pub fn install_auto_peer_manager_hooks(&self) {
        let payments = self.payments.clone();
        let peers = self.peers.clone();
        let channels = self.channels.clone();
        let pending_peer_hook_events = self.pending_peer_hook_events.clone();
        let runtime_events = self.runtime_events.clone();
        let next_runtime_event_seq = self.next_runtime_event_seq.clone();
        let runtime_event_store_key = self.runtime_event_store_key.clone();
        let ldk_runtime = self.ldk_runtime.clone();
        let use_runtime_state_for_ln_views = self.use_runtime_state_for_ln_views();
        install_rln_ldk_peer_manager_hooks(RlnLdkPeerManagerHooks {
            new_outbound_connection: Rc::new(move |_peer_pubkey| Ok(String::new())),
            read_event: Rc::new({
                let pending_peer_hook_events = pending_peer_hook_events.clone();
                move |payload_hex| {
                    pending_peer_hook_events
                        .borrow_mut()
                        .push(PendingPeerHookEvent::Payload(payload_hex.to_string()));
                    Ok(())
                }
            }),
            process_events: Rc::new({
                let payments = payments.clone();
                let peers = peers.clone();
                let channels = channels.clone();
                let pending_peer_hook_events = pending_peer_hook_events.clone();
                let runtime_events = runtime_events.clone();
                let next_runtime_event_seq = next_runtime_event_seq.clone();
                let runtime_event_store_key = runtime_event_store_key.clone();
                let ldk_runtime = ldk_runtime.clone();
                move || {
                    let _ = drain_pending_peer_hook_events(
                        &ldk_runtime,
                        use_runtime_state_for_ln_views,
                        &peers,
                        &channels,
                        &payments,
                        &pending_peer_hook_events,
                        &runtime_events,
                        &next_runtime_event_seq,
                        "peer_hook",
                    )?;
                    persist_runtime_event_log_state(
                        &runtime_event_store_key,
                        &runtime_events,
                        &next_runtime_event_seq,
                    );
                    Ok(())
                }
            }),
            socket_disconnected: Rc::new({
                let pending_peer_hook_events = pending_peer_hook_events.clone();
                move || {
                    pending_peer_hook_events
                        .borrow_mut()
                        .push(PendingPeerHookEvent::SocketDisconnected);
                    Ok(())
                }
            }),
            report_error: Rc::new({
                let pending_peer_hook_events = pending_peer_hook_events.clone();
                move |message| {
                    pending_peer_hook_events
                        .borrow_mut()
                        .push(PendingPeerHookEvent::Error(message.to_string()));
                    Ok(())
                }
            }),
        });
    }

    #[wasm_bindgen(js_name = clearAutoPeerManagerHooks)]
    pub fn clear_auto_peer_manager_hooks(&self) {
        clear_rln_ldk_peer_manager_hooks();
    }

    #[wasm_bindgen(js_name = listRuntimeEventsValue)]
    pub fn list_runtime_events_value(&self) -> Result<JsValue, JsValue> {
        let mut events = self.runtime_events.borrow().clone();
        events.sort_by(|a, b| a.seq.cmp(&b.seq));
        crate::js_obj(&events)
    }

    #[wasm_bindgen(js_name = listRuntimeEventsJson)]
    pub fn list_runtime_events_json(&self) -> Result<String, JsValue> {
        let value = self.list_runtime_events_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = failPendingPayments)]
    pub fn fail_pending_payments_api(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        ensure_manual_status_update_allowed(self.use_runtime_state_for_ln_views())?;
        if self.use_runtime_state_for_ln_views() {
            let pending_hashes = self
                .ldk_runtime
                .list_payments()
                .into_iter()
                .filter(|p| p.status == "pending")
                .map(|p| p.payment_hash)
                .collect::<Vec<_>>();
            for payment_hash in pending_hashes {
                let _ = self.apply_payment_status_via_event_stream(
                    &payment_hash,
                    "failed",
                    "manual_api",
                )?;
            }
        } else {
            fail_pending_payments_with_runtime_events(
                &self.payments,
                &self.runtime_events,
                &self.next_runtime_event_seq,
                "manual_api",
                "failed",
            )?;
        }
        self.persist_runtime_event_log_state();
        self.list_payments_value()
    }

    #[wasm_bindgen(js_name = sendPaymentValue)]
    pub fn send_payment_value(
        &self,
        invoice: String,
        amt_msat: Option<u64>,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let invoice = invoice.trim().to_string();
        if invoice.is_empty() {
            return Err(JsValue::from_str("invoice cannot be empty"));
        }
        if (asset_id.is_some() && asset_amount.is_none())
            || (asset_id.is_none() && asset_amount.is_some())
        {
            return Err(JsValue::from_str(
                "asset_id and asset_amount must be provided together",
            ));
        }
        if let Some(id) = &asset_id {
            if id.trim().is_empty() {
                return Err(JsValue::from_str("asset_id cannot be empty if provided"));
            }
            validate_asset_id_format(id)?;
        }
        if let Some(amount) = asset_amount {
            if amount == 0 {
                return Err(JsValue::from_str("asset_amount must be > 0 when provided"));
            }
        }
        if let Some(msat) = amt_msat {
            if msat == 0 {
                return Err(JsValue::from_str("amt_msat must be > 0 when provided"));
            }
        }

        let parsed = Bolt11Invoice::from_str(&invoice)
            .map_err(|e| JsValue::from_str(&format!("invalid invoice: {e}")))?;
        let payment_hash = parsed.payment_hash().to_string();
        let payment_id = payment_hash.clone();
        let invoice_amt_msat = parsed.amount_milli_satoshis();
        let zero_amt_invoice = invoice_amt_msat.is_none() || invoice_amt_msat == Some(0);
        if zero_amt_invoice && amt_msat.is_none() {
            return Err(JsValue::from_str(
                "need an amount for the given 0-value invoice",
            ));
        }
        let resolved_amt_msat = match (amt_msat, invoice_amt_msat) {
            (Some(requested), Some(from_invoice)) if requested != from_invoice => {
                return Err(JsValue::from_str(&format!(
                    "amount didn't match invoice value of {from_invoice}msat"
                )));
            }
            (Some(requested), _) => Some(requested),
            (None, Some(from_invoice)) => Some(from_invoice),
            (None, None) => None,
        };
        if (asset_id.is_some() || asset_amount.is_some())
            && resolved_amt_msat.unwrap_or(0) < SDK_INVOICE_MIN_MSAT
        {
            return Err(JsValue::from_str(&format!(
                "amt_msat in invoice sending an RGB asset cannot be less than {SDK_INVOICE_MIN_MSAT}"
            )));
        }
        let now = unix_now_secs();
        let payee_pubkey = parsed
            .payee_pub_key()
            .copied()
            .unwrap_or_else(|| parsed.recover_payee_pub_key())
            .to_string();
        let has_connected_peer = if self.use_runtime_state_for_ln_views() {
            if self.ldk_runtime.get_peer(&payee_pubkey).is_some() {
                self.has_connected_peer(&payee_pubkey)
            } else {
                self.has_any_connected_peer()
            }
        } else if self.peers.borrow().contains_key(&payee_pubkey) {
            self.has_connected_peer(&payee_pubkey)
        } else {
            self.has_any_connected_peer()
        };

        let data = RlnWasmNodePaymentData {
            amt_msat: resolved_amt_msat,
            asset_amount,
            asset_id,
            payment_hash: payment_hash.clone(),
            inbound: false,
            status: "pending".to_string(),
            created_at: now,
            updated_at: now,
            payee_pubkey,
        };

        self.payments
            .borrow_mut()
            .insert(payment_hash.clone(), PaymentEntry { data });
        if self.use_runtime_state_for_ln_views() {
            let runtime_payment = self
                .payments
                .borrow()
                .get(&payment_hash)
                .map(|entry| Self::payment_runtime_state_from_data(&entry.data))
                .ok_or_else(|| JsValue::from_str("payment not found after creation"))?;
            self.ldk_runtime.upsert_payment(runtime_payment);
        }
        if !has_connected_peer {
            let _ =
                self.apply_payment_status_via_event_stream(&payment_hash, "failed", "node_api")?;
        }
        self.persist_runtime_event_log_state();
        let final_status = self
            .payments
            .borrow()
            .get(&payment_hash)
            .map(|entry| entry.data.status.clone())
            .ok_or_else(|| JsValue::from_str("payment not found after creation"))?;

        crate::js_obj(&RlnWasmNodeSendPaymentResult {
            payment_id,
            payment_hash: Some(payment_hash),
            payment_secret: Some(hex::encode(parsed.payment_secret().0)),
            status: final_status,
        })
    }

    #[wasm_bindgen(js_name = sendPaymentJson)]
    pub fn send_payment_json(
        &self,
        invoice: String,
        amt_msat: Option<u64>,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        let value = self.send_payment_value(invoice, amt_msat, asset_id, asset_amount)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = keysendValue)]
    pub fn keysend_value(
        &self,
        dest_pubkey: String,
        amt_msat: u64,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let dest_pubkey = dest_pubkey.trim().to_string();
        if dest_pubkey.trim().is_empty() {
            return Err(JsValue::from_str("dest_pubkey cannot be empty"));
        }
        if SecpPublicKey::from_str(dest_pubkey.trim()).is_err() {
            return Err(JsValue::from_str("invalid dest_pubkey"));
        }
        if amt_msat < SDK_HTLC_MIN_MSAT {
            return Err(JsValue::from_str(&format!(
                "amt_msat cannot be less than {SDK_HTLC_MIN_MSAT}"
            )));
        }
        if (asset_id.is_some() && asset_amount.is_none())
            || (asset_id.is_none() && asset_amount.is_some())
        {
            return Err(JsValue::from_str(
                "asset_id and asset_amount must be provided together",
            ));
        }
        if let Some(id) = &asset_id {
            if id.trim().is_empty() {
                return Err(JsValue::from_str("asset_id cannot be empty if provided"));
            }
            validate_asset_id_format(id)?;
        }
        if let Some(amount) = asset_amount {
            if amount == 0 {
                return Err(JsValue::from_str("asset_amount must be > 0 when provided"));
            }
        }

        let (_payment_id, payment_hash) = self.next_payment_identity();
        let payment_preimage = format!("{:064x}", self.next_payment_number());
        let now = unix_now_secs();
        let has_connected_peer = self.has_connected_peer(&dest_pubkey);

        let data = RlnWasmNodePaymentData {
            amt_msat: Some(amt_msat),
            asset_amount,
            asset_id,
            payment_hash: payment_hash.clone(),
            inbound: false,
            status: "pending".to_string(),
            created_at: now,
            updated_at: now,
            payee_pubkey: dest_pubkey,
        };

        self.payments
            .borrow_mut()
            .insert(payment_hash.clone(), PaymentEntry { data });
        if self.use_runtime_state_for_ln_views() {
            let runtime_payment = self
                .payments
                .borrow()
                .get(&payment_hash)
                .map(|entry| Self::payment_runtime_state_from_data(&entry.data))
                .ok_or_else(|| JsValue::from_str("payment not found after keysend"))?;
            self.ldk_runtime.upsert_payment(runtime_payment);
        }
        if !has_connected_peer {
            let _ =
                self.apply_payment_status_via_event_stream(&payment_hash, "failed", "node_api")?;
        }
        self.persist_runtime_event_log_state();
        let final_status = self
            .payments
            .borrow()
            .get(&payment_hash)
            .map(|entry| entry.data.status.clone())
            .ok_or_else(|| JsValue::from_str("payment not found after keysend"))?;

        crate::js_obj(&RlnWasmNodeKeysendResult {
            payment_hash,
            payment_preimage,
            status: final_status,
        })
    }

    #[wasm_bindgen(js_name = keysendJson)]
    pub fn keysend_json(
        &self,
        dest_pubkey: String,
        amt_msat: u64,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        let value = self.keysend_value(dest_pubkey, amt_msat, asset_id, asset_amount)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = listPaymentsValue)]
    pub fn list_payments_value(&self) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let mut data = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .list_payments()
                .into_iter()
                .map(Self::payment_data_from_runtime_state)
                .collect::<Vec<_>>()
        } else {
            self.payments
                .borrow()
                .values()
                .map(|entry| entry.data.clone())
                .collect::<Vec<_>>()
        };
        data.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.payment_hash.cmp(&b.payment_hash))
        });
        crate::js_obj(&data)
    }

    #[wasm_bindgen(js_name = listPaymentsJson)]
    pub fn list_payments_json(&self) -> Result<String, JsValue> {
        let value = self.list_payments_value()?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = getPaymentValue)]
    pub fn get_payment_value(&self, payment_hash: String) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let payment_hash = payment_hash.trim().to_string();
        if payment_hash.is_empty() {
            return Err(JsValue::from_str("payment_hash cannot be empty"));
        }
        let data = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .get_payment(&payment_hash)
                .map(Self::payment_data_from_runtime_state)
                .ok_or_else(|| JsValue::from_str("payment not found"))?
        } else {
            self.payments
                .borrow()
                .get(&payment_hash)
                .map(|entry| entry.data.clone())
                .ok_or_else(|| JsValue::from_str("payment not found"))?
        };
        crate::js_obj(&data)
    }

    #[wasm_bindgen(js_name = getPaymentJson)]
    pub fn get_payment_json(&self, payment_hash: String) -> Result<String, JsValue> {
        let value = self.get_payment_value(payment_hash)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = updatePaymentStatus)]
    pub fn update_payment_status(
        &self,
        payment_hash: String,
        status: String,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        ensure_manual_status_update_allowed(self.use_runtime_state_for_ln_views())?;
        let payment_hash = payment_hash.trim().to_string();
        if payment_hash.is_empty() {
            return Err(JsValue::from_str("payment_hash cannot be empty"));
        }
        let normalized = normalize_payment_status(&status)?;
        let data =
            self.apply_and_record_payment_status_event(&payment_hash, &normalized, "node_api")?;
        self.persist_runtime_event_log_state();
        crate::js_obj(&data)
    }

    #[wasm_bindgen(js_name = updatePaymentStatusJson)]
    pub fn update_payment_status_json(
        &self,
        payment_hash: String,
        status: String,
    ) -> Result<String, JsValue> {
        let value = self.update_payment_status(payment_hash, status)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = decodeLnInvoiceValue)]
    pub fn decode_ln_invoice_value(&self, invoice: String) -> Result<JsValue, JsValue> {
        let invoice = invoice.trim();
        if invoice.is_empty() {
            return Err(JsValue::from_str("invoice cannot be empty"));
        }
        let parsed = Bolt11Invoice::from_str(invoice)
            .map_err(|e| JsValue::from_str(&format!("invalid invoice: {e}")))?;
        let data = RlnWasmNodeDecodeLnInvoiceData {
            amt_msat: parsed.amount_milli_satoshis(),
            expiry_sec: parsed.expiry_time().as_secs(),
            timestamp: parsed.duration_since_epoch().as_secs(),
            asset_id: None,
            asset_amount: None,
            payment_hash: parsed.payment_hash().to_string(),
            payment_secret: hex::encode(parsed.payment_secret().0),
            payee_pubkey: parsed.payee_pub_key().map(|p| p.to_string()),
            network: format!("{:?}", parsed.network()).to_lowercase(),
        };
        crate::js_obj(&data)
    }

    #[wasm_bindgen(js_name = decodeLnInvoiceJson)]
    pub fn decode_ln_invoice_json(&self, invoice: String) -> Result<String, JsValue> {
        let value = self.decode_ln_invoice_value(invoice)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = decodeRgbInvoiceValue)]
    pub fn decode_rgb_invoice_value(&self, invoice: String) -> Result<JsValue, JsValue> {
        let invoice = invoice.trim();
        if invoice.is_empty() {
            return Err(JsValue::from_str("invoice cannot be empty"));
        }
        let parsed = rgb_lib_wasm::wallet::Invoice::new(invoice.to_string())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        crate::js_obj(&parsed.invoice_data())
    }

    #[wasm_bindgen(js_name = decodeRgbInvoiceJson)]
    pub fn decode_rgb_invoice_json(&self, invoice: String) -> Result<String, JsValue> {
        let value = self.decode_rgb_invoice_value(invoice)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = createLnInvoiceValue)]
    pub fn create_ln_invoice_value(
        &self,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        if expiry_sec == 0 {
            return Err(JsValue::from_str("expiry_sec must be > 0"));
        }
        if let Some(msat) = amt_msat {
            if msat == 0 {
                return Err(JsValue::from_str("amt_msat must be > 0 when provided"));
            }
        }
        if (asset_id.is_some() && asset_amount.is_none())
            || (asset_id.is_none() && asset_amount.is_some())
        {
            return Err(JsValue::from_str(
                "asset_id and asset_amount must be provided together",
            ));
        }
        if let Some(id) = &asset_id {
            if id.trim().is_empty() {
                return Err(JsValue::from_str("asset_id cannot be empty if provided"));
            }
            validate_asset_id_format(id)?;
        }
        if let Some(amount) = asset_amount {
            if amount == 0 {
                return Err(JsValue::from_str("asset_amount must be > 0 when provided"));
            }
        }
        if asset_id.is_some() && amt_msat.unwrap_or(0) < SDK_INVOICE_MIN_MSAT {
            return Err(JsValue::from_str(&format!(
                "amt_msat cannot be less than {SDK_INVOICE_MIN_MSAT} when transferring an RGB asset"
            )));
        }

        let (payment_hash, payment_secret) = self.next_invoice_payment_identity();
        let now = unix_now_secs();
        let (node_secret_key, node_pubkey) = self.scaffold_node_signing_identity()?;
        let currency = self.invoice_currency()?;

        let mut builder = InvoiceBuilder::new(currency)
            .description("rln-wasm-sdk".to_string())
            .payment_hash(payment_hash)
            .payment_secret(payment_secret)
            .current_timestamp()
            .min_final_cltv_expiry_delta(18)
            .expiry_time(Duration::from_secs(expiry_sec as u64));
        if let Some(msat) = amt_msat {
            builder = builder.amount_milli_satoshis(msat);
        }
        let secp_ctx = Secp256k1::new();
        let invoice = builder
            .build_signed(|hash| secp_ctx.sign_ecdsa_recoverable(hash, &node_secret_key))
            .map_err(|e| JsValue::from_str(&format!("failed to create invoice: {e}")))?;

        let payment_hash_hex = invoice.payment_hash().to_string();
        let data = RlnWasmNodePaymentData {
            amt_msat: invoice.amount_milli_satoshis(),
            asset_amount,
            asset_id,
            payment_hash: payment_hash_hex.clone(),
            inbound: true,
            status: "pending".to_string(),
            created_at: now,
            updated_at: now,
            payee_pubkey: node_pubkey.to_string(),
        };
        self.payments
            .borrow_mut()
            .insert(payment_hash_hex, PaymentEntry { data });
        if self.use_runtime_state_for_ln_views() {
            let runtime_payment = self
                .payments
                .borrow()
                .get(&invoice.payment_hash().to_string())
                .map(|entry| Self::payment_runtime_state_from_data(&entry.data))
                .ok_or_else(|| JsValue::from_str("payment not found after invoice creation"))?;
            self.ldk_runtime.upsert_payment(runtime_payment);
        }

        crate::js_obj(&RlnWasmNodeCreateLnInvoiceData {
            invoice: invoice.to_string(),
        })
    }

    #[wasm_bindgen(js_name = createLnInvoiceJson)]
    pub fn create_ln_invoice_json(
        &self,
        amt_msat: Option<u64>,
        expiry_sec: u32,
        asset_id: Option<String>,
        asset_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        let value = self.create_ln_invoice_value(amt_msat, expiry_sec, asset_id, asset_amount)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = invoiceStatusValue)]
    pub fn invoice_status_value(&self, invoice: String) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let invoice = invoice.trim();
        if invoice.is_empty() {
            return Err(JsValue::from_str("invoice cannot be empty"));
        }
        let parsed = Bolt11Invoice::from_str(invoice)
            .map_err(|e| JsValue::from_str(&format!("invalid invoice: {e}")))?;
        let payment_hash = parsed.payment_hash().to_string();
        let payment = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .get_payment(&payment_hash)
                .map(Self::payment_data_from_runtime_state)
                .ok_or_else(|| JsValue::from_str("unknown LN invoice"))?
        } else {
            self.payments
                .borrow()
                .get(&payment_hash)
                .map(|entry| entry.data.clone())
                .ok_or_else(|| JsValue::from_str("unknown LN invoice"))?
        };
        if payment.status == "pending" && parsed.is_expired() {
            let _ =
                self.apply_payment_status_via_event_stream(&payment_hash, "expired", "node_api")?;
        }
        let status = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .get_payment(&payment_hash)
                .map(|entry| entry.status)
                .ok_or_else(|| JsValue::from_str("unknown LN invoice"))?
        } else {
            self.payments
                .borrow()
                .get(&payment_hash)
                .map(|entry| entry.data.status.clone())
                .ok_or_else(|| JsValue::from_str("unknown LN invoice"))?
        };
        crate::js_obj(&RlnWasmNodeInvoiceStatusData { status })
    }

    #[wasm_bindgen(js_name = invoiceStatusJson)]
    pub fn invoice_status_json(&self, invoice: String) -> Result<String, JsValue> {
        let value = self.invoice_status_value(invoice)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = updatePaymentStatusByInvoice)]
    pub fn update_payment_status_by_invoice(
        &self,
        invoice: String,
        status: String,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        ensure_manual_status_update_allowed(self.use_runtime_state_for_ln_views())?;
        let invoice = invoice.trim();
        if invoice.is_empty() {
            return Err(JsValue::from_str("invoice cannot be empty"));
        }
        let parsed = Bolt11Invoice::from_str(invoice)
            .map_err(|e| JsValue::from_str(&format!("invalid invoice: {e}")))?;
        let payment_hash = parsed.payment_hash().to_string();
        self.update_payment_status(payment_hash, status)
    }

    #[wasm_bindgen(js_name = updatePaymentStatusByInvoiceJson)]
    pub fn update_payment_status_by_invoice_json(
        &self,
        invoice: String,
        status: String,
    ) -> Result<String, JsValue> {
        let value = self.update_payment_status_by_invoice(invoice, status)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = ingestReadEventPayloadHex)]
    pub fn ingest_read_event_payload_hex(&self, payload_hex: String) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        ensure_manual_event_ingestion_allowed(self.use_runtime_state_for_ln_views())?;
        if self.use_runtime_state_for_ln_views() {
            let received_at = unix_now_secs();
            let seq = next_runtime_event_seq(&self.next_runtime_event_seq);
            let Some(event) = parse_payment_status_event_payload(&payload_hex) else {
                let error =
                    "unrecognized event payload format for payment status update".to_string();
                let event_kind = classify_non_payment_payload_kind(&payload_hex);
                record_runtime_event(
                    &self.runtime_events,
                    RlnWasmNodeRuntimeEventData {
                        seq,
                        source: "manual_api".to_string(),
                        event_kind,
                        payload_hex,
                        payment_hash: None,
                        status: None,
                        applied: false,
                        error: Some(error.clone()),
                        received_at,
                    },
                );
                self.persist_runtime_event_log_state();
                return Err(JsValue::from_str(&error));
            };
            let result = self
                .apply_and_record_payment_status_event(
                    &event.payment_hash,
                    &event.status,
                    "manual_api",
                )
                .and_then(|payment| crate::js_obj(&payment));
            self.persist_runtime_event_log_state();
            return result;
        }
        let maybe_payment = apply_runtime_event_payload(
            &self.payments,
            &self.runtime_events,
            &self.next_runtime_event_seq,
            payload_hex,
            "manual_api",
            RuntimeEventApplyMode::StrictPaymentStatus,
        )?;
        self.persist_runtime_event_log_state();
        let payment = maybe_payment.ok_or_else(|| JsValue::from_str("payment not found"))?;
        crate::js_obj(&payment)
    }

    #[wasm_bindgen(js_name = ingestReadEventPayloadHexJson)]
    pub fn ingest_read_event_payload_hex_json(
        &self,
        payload_hex: String,
    ) -> Result<String, JsValue> {
        let value = self.ingest_read_event_payload_hex(payload_hex)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = ingestRuntimeTransportEventPayloadHexValue)]
    pub fn ingest_runtime_transport_event_payload_hex_value(
        &self,
        payload_hex: String,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        ensure_manual_event_ingestion_allowed(self.use_runtime_state_for_ln_views())?;
        let result = self.apply_and_record_transport_event_from_payload_hex(
            payload_hex,
            "runtime_transport_api",
        )?;
        self.persist_runtime_event_log_state();
        crate::js_obj(&result)
    }

    #[wasm_bindgen(js_name = ingestRuntimeTransportEventPayloadHexJson)]
    pub fn ingest_runtime_transport_event_payload_hex_json(
        &self,
        payload_hex: String,
    ) -> Result<String, JsValue> {
        let value = self.ingest_runtime_transport_event_payload_hex_value(payload_hex)?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = openChannelValue)]
    pub fn open_channel_value(
        &self,
        peer_pubkey: String,
        capacity_sat: u64,
        public: bool,
        asset_id: Option<String>,
        asset_local_amount: Option<u64>,
    ) -> Result<JsValue, JsValue> {
        self.ensure_runtime_ready()?;
        let peer_pubkey = peer_pubkey.trim().to_string();
        if peer_pubkey.trim().is_empty() {
            return Err(JsValue::from_str("peer_pubkey cannot be empty"));
        }
        if SecpPublicKey::from_str(peer_pubkey.trim()).is_err() {
            return Err(JsValue::from_str("invalid peer_pubkey"));
        }
        if let Some(id) = &asset_id {
            if id.trim().is_empty() {
                return Err(JsValue::from_str("asset_id cannot be empty if provided"));
            }
        }
        let has_rgb = match (&asset_id, asset_local_amount) {
            (Some(_), Some(amount)) if amount < SDK_OPENCHANNEL_MIN_RGB_AMT => {
                return Err(JsValue::from_str(&format!(
                    "Channel RGB amount must be equal to or higher than {SDK_OPENCHANNEL_MIN_RGB_AMT}"
                )));
            }
            (Some(id), Some(_)) => {
                validate_asset_id_format(id)?;
                true
            }
            (None, None) => false,
            _ => {
                return Err(JsValue::from_str(
                    "asset_id and asset_local_amount must be provided together",
                ));
            }
        };
        if has_rgb && capacity_sat < SDK_OPENRGBCHANNEL_MIN_SAT {
            return Err(JsValue::from_str(&format!(
                "RGB channel amount must be equal to or higher than {SDK_OPENRGBCHANNEL_MIN_SAT} sats"
            )));
        }
        if !has_rgb && capacity_sat < SDK_OPENCHANNEL_MIN_SAT {
            return Err(JsValue::from_str(&format!(
                "Channel amount must be equal to or higher than {SDK_OPENCHANNEL_MIN_SAT} sats"
            )));
        }
        if capacity_sat > SDK_OPENCHANNEL_MAX_SAT {
            return Err(JsValue::from_str(&format!(
                "Channel amount must be equal to or less than {SDK_OPENCHANNEL_MAX_SAT} sats"
            )));
        }
        let has_peer = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .get_peer(&peer_pubkey)
                .map(|peer| peer.started)
                .unwrap_or(false)
        } else {
            self.peers.borrow().contains_key(&peer_pubkey)
        };
        if !has_peer {
            return Err(JsValue::from_str("peer is not connected"));
        }

        let mut seq = self.next_channel_seq.borrow_mut();
        *seq += 1;
        let next = *seq;

        let temporary_channel_id = format!("wasm-tmp:{}:{}", peer_pubkey, next);
        let channel_id = format!("wasm-chan:{}:{}", peer_pubkey, next);
        let data = RlnWasmNodeChannelData {
            temporary_channel_id: temporary_channel_id.clone(),
            channel_id: channel_id.clone(),
            peer_pubkey,
            status: "pending".to_string(),
            ready: false,
            is_usable: false,
            public,
            capacity_sat,
            asset_id,
            asset_local_amount,
        };

        if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .upsert_channel(Self::channel_runtime_state_from_data(&data));
        } else {
            self.channels.borrow_mut().insert(
                channel_id.clone(),
                ChannelEntry {
                    temporary_channel_id,
                    data: data.clone(),
                },
            );
        }
        let applied = self
            .apply_and_record_transport_event(
                RuntimeTransportEvent::ChannelUsable {
                    channel_id: channel_id.clone(),
                },
                "node_api",
            )?
            .applied;
        if !applied {
            return Err(JsValue::from_str(
                "failed to apply channel_usable transport event",
            ));
        }
        self.persist_runtime_event_log_state();
        let channel = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .list_channels()
                .into_iter()
                .find(|entry| entry.channel_id == channel_id)
                .map(Self::channel_data_from_runtime_state)
                .ok_or_else(|| JsValue::from_str("channel not found after open"))?
        } else {
            self.channels
                .borrow()
                .get(&channel_id)
                .map(|entry| entry.data.clone())
                .ok_or_else(|| JsValue::from_str("channel not found after open"))?
        };
        crate::js_obj(&channel)
    }

    #[wasm_bindgen(js_name = openChannelJson)]
    pub fn open_channel_json(
        &self,
        peer_pubkey: String,
        capacity_sat: u64,
        public: bool,
        asset_id: Option<String>,
        asset_local_amount: Option<u64>,
    ) -> Result<String, JsValue> {
        let value = self.open_channel_value(
            peer_pubkey,
            capacity_sat,
            public,
            asset_id,
            asset_local_amount,
        )?;
        let parsed: serde_json::Value = crate::js_from(value)?;
        crate::js_to_json(&parsed)
    }

    #[wasm_bindgen(js_name = closeChannel)]
    pub fn close_channel(&self, channel_id: String) -> Result<(), JsValue> {
        self.ensure_runtime_ready()?;
        if channel_id.trim().is_empty() {
            return Err(JsValue::from_str("channel_id cannot be empty"));
        }
        let applied = self
            .apply_and_record_transport_event(
                RuntimeTransportEvent::ChannelClosed {
                    channel_id: channel_id.clone(),
                },
                "node_api",
            )?
            .applied;
        if !applied {
            return Err(JsValue::from_str("channel not found"));
        }
        self.persist_runtime_event_log_state();
        Ok(())
    }

    #[wasm_bindgen(js_name = getChannelId)]
    pub fn get_channel_id(&self, temporary_channel_id: String) -> Result<String, JsValue> {
        self.ensure_runtime_ready()?;
        if temporary_channel_id.trim().is_empty() {
            return Err(JsValue::from_str("temporary_channel_id cannot be empty"));
        }
        let channel_id = if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .find_channel_by_temporary(&temporary_channel_id)
                .ok_or_else(|| JsValue::from_str("unknown temporary channel ID"))?
        } else {
            self.channels
                .borrow()
                .values()
                .find(|entry| entry.temporary_channel_id == temporary_channel_id)
                .map(|entry| entry.data.channel_id.clone())
                .ok_or_else(|| JsValue::from_str("unknown temporary channel ID"))?
        };
        Ok(channel_id)
    }
}

impl RlnWasmNode {
    fn persist_runtime_event_log_state(&self) {
        persist_runtime_event_log_state(
            &self.runtime_event_store_key,
            &self.runtime_events,
            &self.next_runtime_event_seq,
        );
    }

    fn use_runtime_state_for_ln_views(&self) -> bool {
        self.ldk_runtime.status().backend == "ldk_bridge"
    }

    fn has_any_connected_peer(&self) -> bool {
        if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime.has_any_connected_peer()
        } else {
            self.peers
                .borrow()
                .values()
                .any(|entry| entry.session.is_started())
        }
    }

    fn has_connected_peer(&self, peer_pubkey: &str) -> bool {
        if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime.has_connected_peer(peer_pubkey)
        } else {
            self.peers
                .borrow()
                .get(peer_pubkey)
                .map(|entry| entry.session.is_started())
                .unwrap_or(false)
        }
    }

    #[cfg(test)]
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn test_upsert_runtime_peer(
        &self,
        pubkey: String,
        peer_addr: String,
        started: bool,
    ) {
        self.ldk_runtime.upsert_peer(LdkRuntimePeerStateData {
            pubkey,
            peer_addr,
            started,
        });
    }

    #[cfg(test)]
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn test_set_runtime_peer_started(&self, pubkey: &str, started: bool) -> bool {
        self.ldk_runtime.set_peer_started(pubkey, started)
    }

    fn channel_runtime_state_from_data(
        data: &RlnWasmNodeChannelData,
    ) -> LdkRuntimeChannelStateData {
        LdkRuntimeChannelStateData {
            temporary_channel_id: data.temporary_channel_id.clone(),
            channel_id: data.channel_id.clone(),
            peer_pubkey: data.peer_pubkey.clone(),
            status: data.status.clone(),
            ready: data.ready,
            is_usable: data.is_usable,
            public: data.public,
            capacity_sat: data.capacity_sat,
            asset_id: data.asset_id.clone(),
            asset_local_amount: data.asset_local_amount,
        }
    }

    fn channel_data_from_runtime_state(
        state: LdkRuntimeChannelStateData,
    ) -> RlnWasmNodeChannelData {
        RlnWasmNodeChannelData {
            temporary_channel_id: state.temporary_channel_id,
            channel_id: state.channel_id,
            peer_pubkey: state.peer_pubkey,
            status: state.status,
            ready: state.ready,
            is_usable: state.is_usable,
            public: state.public,
            capacity_sat: state.capacity_sat,
            asset_id: state.asset_id,
            asset_local_amount: state.asset_local_amount,
        }
    }

    fn payment_runtime_state_from_data(
        data: &RlnWasmNodePaymentData,
    ) -> LdkRuntimePaymentStateData {
        LdkRuntimePaymentStateData {
            amt_msat: data.amt_msat,
            asset_amount: data.asset_amount,
            asset_id: data.asset_id.clone(),
            payment_hash: data.payment_hash.clone(),
            inbound: data.inbound,
            status: data.status.clone(),
            created_at: data.created_at,
            updated_at: data.updated_at,
            payee_pubkey: data.payee_pubkey.clone(),
        }
    }

    fn payment_data_from_runtime_state(
        state: LdkRuntimePaymentStateData,
    ) -> RlnWasmNodePaymentData {
        RlnWasmNodePaymentData {
            amt_msat: state.amt_msat,
            asset_amount: state.asset_amount,
            asset_id: state.asset_id,
            payment_hash: state.payment_hash,
            inbound: state.inbound,
            status: state.status,
            created_at: state.created_at,
            updated_at: state.updated_at,
            payee_pubkey: state.payee_pubkey,
        }
    }

    fn apply_payment_status_via_event_stream(
        &self,
        payment_hash: &str,
        status: &str,
        source: &str,
    ) -> Result<RlnWasmNodePaymentData, JsValue> {
        let payload_hex = encode_payment_status_event_payload(payment_hash, status);
        apply_runtime_hook_payload(
            &self.ldk_runtime,
            self.use_runtime_state_for_ln_views(),
            &self.peers,
            &self.channels,
            &self.payments,
            &self.runtime_events,
            &self.next_runtime_event_seq,
            payload_hex,
            source,
        )?;
        self.persist_runtime_event_log_state();
        if self.use_runtime_state_for_ln_views() {
            self.ldk_runtime
                .get_payment(payment_hash)
                .map(Self::payment_data_from_runtime_state)
                .ok_or_else(|| JsValue::from_str("payment not found"))
        } else {
            self.payments
                .borrow()
                .get(payment_hash)
                .map(|entry| entry.data.clone())
                .ok_or_else(|| JsValue::from_str("payment not found"))
        }
    }

    fn apply_and_record_payment_status_event(
        &self,
        payment_hash: &str,
        status: &str,
        source: &str,
    ) -> Result<RlnWasmNodePaymentData, JsValue> {
        if self.use_runtime_state_for_ln_views() {
            let received_at = unix_now_secs();
            let seq = next_runtime_event_seq(&self.next_runtime_event_seq);
            let normalized = normalize_payment_status(status)?;
            let Some(mut payment) = self.ldk_runtime.get_payment(payment_hash) else {
                let error = "payment not found".to_string();
                record_runtime_event(
                    &self.runtime_events,
                    RlnWasmNodeRuntimeEventData {
                        seq,
                        source: source.to_string(),
                        event_kind: "payment_status".to_string(),
                        payload_hex: encode_payment_status_event_payload(payment_hash, &normalized),
                        payment_hash: Some(payment_hash.to_string()),
                        status: Some(normalized),
                        applied: false,
                        error: Some(error.clone()),
                        received_at,
                    },
                );
                self.persist_runtime_event_log_state();
                return Err(JsValue::from_str(&error));
            };
            if !is_valid_payment_status_transition(&payment.status, &normalized) {
                let error = format!(
                    "invalid payment status transition: {} -> {}",
                    payment.status, normalized
                );
                record_runtime_event(
                    &self.runtime_events,
                    RlnWasmNodeRuntimeEventData {
                        seq,
                        source: source.to_string(),
                        event_kind: "payment_status".to_string(),
                        payload_hex: encode_payment_status_event_payload(payment_hash, &normalized),
                        payment_hash: Some(payment_hash.to_string()),
                        status: Some(normalized),
                        applied: false,
                        error: Some(error.clone()),
                        received_at,
                    },
                );
                self.persist_runtime_event_log_state();
                return Err(JsValue::from_str(&error));
            }
            payment.status = normalized.clone();
            payment.updated_at = unix_now_secs();
            self.ldk_runtime.upsert_payment(payment.clone());
            record_runtime_event(
                &self.runtime_events,
                RlnWasmNodeRuntimeEventData {
                    seq,
                    source: source.to_string(),
                    event_kind: "payment_status".to_string(),
                    payload_hex: encode_payment_status_event_payload(payment_hash, &normalized),
                    payment_hash: Some(payment_hash.to_string()),
                    status: Some(normalized.clone()),
                    applied: true,
                    error: None,
                    received_at,
                },
            );
            crate::swap_runtime::apply_payment_status_update(payment_hash, &normalized);
            self.persist_runtime_event_log_state();
            return Ok(Self::payment_data_from_runtime_state(payment));
        }
        let payload_hex = encode_payment_status_event_payload(payment_hash, status);
        let payment = apply_runtime_event_payload(
            &self.payments,
            &self.runtime_events,
            &self.next_runtime_event_seq,
            payload_hex,
            source,
            RuntimeEventApplyMode::StrictPaymentStatus,
        )?
        .ok_or_else(|| JsValue::from_str("payment not found"))?;
        crate::swap_runtime::apply_payment_status_update(&payment.payment_hash, &payment.status);
        self.persist_runtime_event_log_state();
        Ok(payment)
    }

    fn apply_and_record_transport_event(
        &self,
        event: RuntimeTransportEvent,
        source: &str,
    ) -> Result<RuntimeTransportEventApplyData, JsValue> {
        let payload_hex = encode_transport_event_payload(&event);
        self.apply_and_record_transport_event_from_parsed(event, payload_hex, source)
    }

    fn apply_and_record_transport_event_from_payload_hex(
        &self,
        payload_hex: String,
        source: &str,
    ) -> Result<RuntimeTransportEventApplyData, JsValue> {
        let Some(event) = parse_transport_event_payload(&payload_hex) else {
            let received_at = unix_now_secs();
            let seq = next_runtime_event_seq(&self.next_runtime_event_seq);
            let error = "unrecognized transport event payload format".to_string();
            record_runtime_event(
                &self.runtime_events,
                RlnWasmNodeRuntimeEventData {
                    seq,
                    source: source.to_string(),
                    event_kind: classify_non_payment_payload_kind(&payload_hex),
                    payload_hex,
                    payment_hash: None,
                    status: None,
                    applied: false,
                    error: Some(error.clone()),
                    received_at,
                },
            );
            self.persist_runtime_event_log_state();
            return Err(JsValue::from_str(&error));
        };
        self.apply_and_record_transport_event_from_parsed(event, payload_hex, source)
    }

    fn apply_and_record_transport_event_from_parsed(
        &self,
        event: RuntimeTransportEvent,
        payload_hex: String,
        source: &str,
    ) -> Result<RuntimeTransportEventApplyData, JsValue> {
        let received_at = unix_now_secs();
        let seq = next_runtime_event_seq(&self.next_runtime_event_seq);
        let event_kind = event.event_kind().to_string();
        let applied = self.apply_runtime_transport_event(&event);
        record_runtime_event(
            &self.runtime_events,
            RlnWasmNodeRuntimeEventData {
                seq,
                source: source.to_string(),
                event_kind: event_kind.clone(),
                payload_hex,
                payment_hash: None,
                status: None,
                applied,
                error: if applied {
                    None
                } else {
                    Some("transport event target not found".to_string())
                },
                received_at,
            },
        );
        self.persist_runtime_event_log_state();
        Ok(RuntimeTransportEventApplyData {
            event_kind,
            applied,
        })
    }

    fn next_payment_number(&self) -> u64 {
        let mut seq = self.next_payment_seq.borrow_mut();
        *seq += 1;
        *seq
    }

    fn next_payment_identity(&self) -> (String, String) {
        let n = self.next_payment_number();
        let payment_id = format!("{:064x}", n);
        let payment_hash = format!("{:064x}", n.saturating_add(1_000_000));
        (payment_id, payment_hash)
    }

    fn next_invoice_payment_identity(&self) -> (Sha256, PaymentSecret) {
        let n = self.next_payment_number();
        let payment_hash = Sha256::hash(format!("invoice-payment-hash:{n}").as_bytes());
        let payment_secret_hash = Sha256::hash(format!("invoice-payment-secret:{n}").as_bytes());
        (
            payment_hash,
            PaymentSecret(payment_secret_hash.to_byte_array()),
        )
    }

    fn scaffold_node_signing_identity(&self) -> Result<(SecretKey, SecpPublicKey), JsValue> {
        let sdk_seed = crate::sdk_node_identity_seed();
        let secret_hash = Sha256::hash(
            format!(
                "node-signing-key:{}:{}",
                sdk_seed.as_deref().unwrap_or("ephemeral"),
                self.proxy_url
            )
            .as_bytes(),
        );
        let secret_key = SecretKey::from_slice(&secret_hash.to_byte_array())
            .map_err(|e| JsValue::from_str(&format!("failed to derive node signing key: {e}")))?;
        let pubkey = SecpPublicKey::from_secret_key(&Secp256k1::new(), &secret_key);
        Ok((secret_key, pubkey))
    }

    fn sign_scaffold_message(&self, message: &str) -> Result<String, JsValue> {
        let (secret_key, _) = self.scaffold_node_signing_identity()?;
        let digest = Sha256::hash(message.as_bytes());
        let msg = SecpMessage::from_digest_slice(&digest.to_byte_array())
            .map_err(|e| JsValue::from_str(&format!("failed to hash message: {e}")))?;
        let signature = Secp256k1::new().sign_ecdsa_recoverable(&msg, &secret_key);
        let (recovery_id, compact) = signature.serialize_compact();
        let mut encoded = [0u8; 65];
        encoded[..64].copy_from_slice(&compact);
        encoded[64] = recovery_id.to_i32() as u8;
        Ok(hex::encode(encoded))
    }

    fn invoice_currency(&self) -> Result<Currency, JsValue> {
        match self.network.borrow().as_str() {
            "mainnet" => Ok(Currency::Bitcoin),
            "testnet" => Ok(Currency::BitcoinTestnet),
            "testnet4" => Ok(Currency::Regtest),
            "signet" => Ok(Currency::Signet),
            "regtest" => Ok(Currency::Regtest),
            other => Err(JsValue::from_str(&format!(
                "unsupported network for invoice creation: {other}"
            ))),
        }
    }

    fn apply_runtime_transport_event(&self, event: &RuntimeTransportEvent) -> bool {
        if self.use_runtime_state_for_ln_views() {
            return match event {
                RuntimeTransportEvent::PeerDisconnected { peer_pubkey } => {
                    let removed_peer = self.ldk_runtime.remove_peer(peer_pubkey);
                    let removed_channels =
                        self.ldk_runtime.remove_channels_by_peer(peer_pubkey) > 0;
                    removed_peer || removed_channels
                }
                RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
                    self.ldk_runtime.has_peer(peer_pubkey)
                }
                RuntimeTransportEvent::ChannelClosed { channel_id } => {
                    self.ldk_runtime.remove_channel(channel_id)
                }
                RuntimeTransportEvent::ChannelUsable { channel_id } => {
                    self.ldk_runtime.set_channel_usable(channel_id, true)
                }
                RuntimeTransportEvent::ChannelUnusable { channel_id } => {
                    self.ldk_runtime.set_channel_usable(channel_id, false)
                }
            };
        }
        match event {
            RuntimeTransportEvent::PeerDisconnected { peer_pubkey } => {
                let removed_peer = self.peers.borrow_mut().remove(peer_pubkey).is_some();
                let mut channels = self.channels.borrow_mut();
                let before = channels.len();
                channels.retain(|_, ch| ch.data.peer_pubkey != *peer_pubkey);
                let removed_channels = channels.len() != before;
                removed_peer || removed_channels
            }
            RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
                self.peers.borrow().contains_key(peer_pubkey)
            }
            RuntimeTransportEvent::ChannelClosed { channel_id } => {
                self.channels.borrow_mut().remove(channel_id).is_some()
            }
            RuntimeTransportEvent::ChannelUsable { channel_id } => {
                let mut guard = self.channels.borrow_mut();
                let Some(channel) = guard.get_mut(channel_id) else {
                    return false;
                };
                channel.data.is_usable = true;
                channel.data.ready = true;
                channel.data.status = "opened".to_string();
                true
            }
            RuntimeTransportEvent::ChannelUnusable { channel_id } => {
                let mut guard = self.channels.borrow_mut();
                let Some(channel) = guard.get_mut(channel_id) else {
                    return false;
                };
                channel.data.is_usable = false;
                channel.data.ready = false;
                channel.data.status = "pending".to_string();
                true
            }
        }
    }
}

fn unix_now_secs() -> u64 {
    (js_sys::Date::now() as u64) / 1000
}

fn normalize_payment_status(status: &str) -> Result<String, JsValue> {
    let normalized = status.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "pending" | "succeeded" | "failed" | "expired" => Ok(normalized),
        _ => Err(JsValue::from_str(
            "status must be one of: pending, succeeded, failed, expired",
        )),
    }
}

fn fail_pending_payments_with_runtime_events(
    payments: &Rc<RefCell<HashMap<String, PaymentEntry>>>,
    runtime_events: &Rc<RefCell<Vec<RlnWasmNodeRuntimeEventData>>>,
    next_runtime_event_seq_ref: &Rc<RefCell<u64>>,
    source: &str,
    status: &str,
) -> Result<usize, JsValue> {
    let pending_hashes = payments
        .borrow()
        .iter()
        .filter(|(_, entry)| entry.data.status == "pending")
        .map(|(hash, _)| hash.clone())
        .collect::<Vec<_>>();
    let mut applied = 0usize;
    for payment_hash in pending_hashes {
        let payload_hex = encode_payment_status_event_payload(&payment_hash, status);
        let result = apply_runtime_event_payload(
            payments,
            runtime_events,
            next_runtime_event_seq_ref,
            payload_hex,
            source,
            RuntimeEventApplyMode::StrictPaymentStatus,
        )?;
        if result.is_some() {
            applied += 1;
        }
    }
    Ok(applied)
}

fn set_payment_status(
    payments: &Rc<RefCell<HashMap<String, PaymentEntry>>>,
    payment_hash: &str,
    status: &str,
) -> Result<(), JsValue> {
    let normalized = normalize_payment_status(status)?;
    let mut guard = payments.borrow_mut();
    let Some(payment) = guard.get_mut(payment_hash) else {
        return Err(JsValue::from_str("payment not found"));
    };
    if !is_valid_payment_status_transition(&payment.data.status, &normalized) {
        return Err(JsValue::from_str(&format!(
            "invalid payment status transition: {} -> {}",
            payment.data.status, normalized
        )));
    }
    payment.data.status = normalized;
    payment.data.updated_at = unix_now_secs();
    Ok(())
}

fn is_valid_payment_status_transition(current: &str, next: &str) -> bool {
    if current == next {
        return true;
    }
    !matches!(current, "succeeded" | "expired")
}

fn next_runtime_event_seq(next_runtime_event_seq: &Rc<RefCell<u64>>) -> u64 {
    let mut guard = next_runtime_event_seq.borrow_mut();
    *guard += 1;
    *guard
}

fn runtime_event_store_key(proxy_url: &str) -> String {
    format!("{RUNTIME_EVENT_LOG_STORAGE_PREFIX}{proxy_url}")
}

fn load_runtime_event_log_snapshot(storage_key: &str) -> Option<RuntimeEventLogSnapshot> {
    let store = browser_persistent_state_store();
    if let Ok(Some(raw)) = store.get(storage_key) {
        if let Ok(snapshot) = serde_json::from_str::<RuntimeEventLogSnapshot>(&raw) {
            RUNTIME_EVENT_LOG_STORAGE.with(|state| {
                state
                    .borrow_mut()
                    .insert(storage_key.to_string(), snapshot.clone());
            });
            return Some(snapshot);
        }
    }

    RUNTIME_EVENT_LOG_STORAGE.with(|state| state.borrow().get(storage_key).cloned())
}

fn persist_runtime_event_log_state(
    storage_key: &str,
    runtime_events: &Rc<RefCell<Vec<RlnWasmNodeRuntimeEventData>>>,
    next_runtime_event_seq_ref: &Rc<RefCell<u64>>,
) {
    let snapshot = RuntimeEventLogSnapshot {
        events: runtime_events.borrow().clone(),
        next_seq: *next_runtime_event_seq_ref.borrow(),
    };
    if let Ok(raw) = serde_json::to_string(&snapshot) {
        let store = browser_persistent_state_store();
        let _ = store.set(storage_key, &raw);
    }
    RUNTIME_EVENT_LOG_STORAGE.with(|state| {
        state.borrow_mut().insert(storage_key.to_string(), snapshot);
    });
}

fn record_runtime_event(
    runtime_events: &Rc<RefCell<Vec<RlnWasmNodeRuntimeEventData>>>,
    entry: RlnWasmNodeRuntimeEventData,
) {
    runtime_events.borrow_mut().push(entry);
}

fn record_runtime_control_event(
    runtime_events: &Rc<RefCell<Vec<RlnWasmNodeRuntimeEventData>>>,
    next_runtime_event_seq_ref: &Rc<RefCell<u64>>,
    source: &str,
    payload_hex: String,
    error: Option<String>,
) {
    let seq = next_runtime_event_seq(next_runtime_event_seq_ref);
    record_runtime_event(
        runtime_events,
        RlnWasmNodeRuntimeEventData {
            seq,
            source: source.to_string(),
            event_kind: "control".to_string(),
            payload_hex,
            payment_hash: None,
            status: None,
            applied: false,
            error,
            received_at: unix_now_secs(),
        },
    );
}

fn apply_runtime_event_payload(
    payments: &Rc<RefCell<HashMap<String, PaymentEntry>>>,
    runtime_events: &Rc<RefCell<Vec<RlnWasmNodeRuntimeEventData>>>,
    next_runtime_event_seq_ref: &Rc<RefCell<u64>>,
    payload_hex: String,
    source: &str,
    mode: RuntimeEventApplyMode,
) -> Result<Option<RlnWasmNodePaymentData>, JsValue> {
    let received_at = unix_now_secs();
    let seq = next_runtime_event_seq(next_runtime_event_seq_ref);
    let Some(event) = parse_payment_status_event_payload(&payload_hex) else {
        let error = "unrecognized event payload format for payment status update".to_string();
        let event_kind = classify_non_payment_payload_kind(&payload_hex);
        record_runtime_event(
            runtime_events,
            RlnWasmNodeRuntimeEventData {
                seq,
                source: source.to_string(),
                event_kind,
                payload_hex,
                payment_hash: None,
                status: None,
                applied: false,
                error: Some(error.clone()),
                received_at,
            },
        );
        return match mode {
            RuntimeEventApplyMode::StrictPaymentStatus => Err(JsValue::from_str(&error)),
            RuntimeEventApplyMode::TolerantTransport => Ok(None),
        };
    };

    if let Err(err) = set_payment_status(payments, &event.payment_hash, &event.status) {
        let error = err
            .as_string()
            .unwrap_or_else(|| "failed to apply runtime event".to_string());
        record_runtime_event(
            runtime_events,
            RlnWasmNodeRuntimeEventData {
                seq,
                source: source.to_string(),
                event_kind: "payment_status".to_string(),
                payload_hex,
                payment_hash: Some(event.payment_hash.clone()),
                status: Some(event.status.clone()),
                applied: false,
                error: Some(error.clone()),
                received_at,
            },
        );
        return match mode {
            RuntimeEventApplyMode::StrictPaymentStatus => Err(JsValue::from_str(&error)),
            RuntimeEventApplyMode::TolerantTransport => Ok(None),
        };
    }

    let payment = payments
        .borrow()
        .get(&event.payment_hash)
        .map(|entry| entry.data.clone());
    record_runtime_event(
        runtime_events,
        RlnWasmNodeRuntimeEventData {
            seq,
            source: source.to_string(),
            event_kind: "payment_status".to_string(),
            payload_hex,
            payment_hash: Some(event.payment_hash),
            status: Some(event.status),
            applied: true,
            error: None,
            received_at,
        },
    );
    Ok(payment)
}

fn apply_runtime_hook_payload(
    ldk_runtime: &Rc<dyn LdkRuntimeManager>,
    use_runtime_state_for_ln_views: bool,
    peers: &Rc<RefCell<HashMap<String, PeerEntry>>>,
    channels: &Rc<RefCell<HashMap<String, ChannelEntry>>>,
    payments: &Rc<RefCell<HashMap<String, PaymentEntry>>>,
    runtime_events: &Rc<RefCell<Vec<RlnWasmNodeRuntimeEventData>>>,
    next_runtime_event_seq_ref: &Rc<RefCell<u64>>,
    payload_hex: String,
    source: &str,
) -> Result<(), JsValue> {
    let received_at = unix_now_secs();
    let seq = next_runtime_event_seq(next_runtime_event_seq_ref);

    if let Some(event) = parse_payment_status_event_payload(&payload_hex) {
        let status = event.status.clone();
        if use_runtime_state_for_ln_views {
            let normalized = normalize_payment_status(&status)?;
            let Some(mut payment) = ldk_runtime.get_payment(&event.payment_hash) else {
                record_runtime_event(
                    runtime_events,
                    RlnWasmNodeRuntimeEventData {
                        seq,
                        source: source.to_string(),
                        event_kind: "payment_status".to_string(),
                        payload_hex,
                        payment_hash: Some(event.payment_hash),
                        status: Some(normalized),
                        applied: false,
                        error: Some("payment not found".to_string()),
                        received_at,
                    },
                );
                return Ok(());
            };
            if !is_valid_payment_status_transition(&payment.status, &normalized) {
                record_runtime_event(
                    runtime_events,
                    RlnWasmNodeRuntimeEventData {
                        seq,
                        source: source.to_string(),
                        event_kind: "payment_status".to_string(),
                        payload_hex,
                        payment_hash: Some(event.payment_hash),
                        status: Some(normalized.clone()),
                        applied: false,
                        error: Some(format!(
                            "invalid payment status transition: {} -> {}",
                            payment.status, normalized
                        )),
                        received_at,
                    },
                );
                return Ok(());
            }
            payment.status = normalized.clone();
            payment.updated_at = unix_now_secs();
            ldk_runtime.upsert_payment(payment);
            crate::swap_runtime::apply_payment_status_update(&event.payment_hash, &normalized);
            record_runtime_event(
                runtime_events,
                RlnWasmNodeRuntimeEventData {
                    seq,
                    source: source.to_string(),
                    event_kind: "payment_status".to_string(),
                    payload_hex,
                    payment_hash: Some(event.payment_hash),
                    status: Some(normalized),
                    applied: true,
                    error: None,
                    received_at,
                },
            );
            return Ok(());
        }

        if let Err(err) = set_payment_status(payments, &event.payment_hash, &status) {
            let error = err
                .as_string()
                .unwrap_or_else(|| "failed to apply runtime event".to_string());
            record_runtime_event(
                runtime_events,
                RlnWasmNodeRuntimeEventData {
                    seq,
                    source: source.to_string(),
                    event_kind: "payment_status".to_string(),
                    payload_hex,
                    payment_hash: Some(event.payment_hash),
                    status: Some(status),
                    applied: false,
                    error: Some(error),
                    received_at,
                },
            );
            return Ok(());
        }
        crate::swap_runtime::apply_payment_status_update(&event.payment_hash, &status);

        record_runtime_event(
            runtime_events,
            RlnWasmNodeRuntimeEventData {
                seq,
                source: source.to_string(),
                event_kind: "payment_status".to_string(),
                payload_hex,
                payment_hash: Some(event.payment_hash),
                status: Some(status),
                applied: true,
                error: None,
                received_at,
            },
        );
        return Ok(());
    }

    if let Some(event) = parse_transport_event_payload(&payload_hex) {
        let applied = apply_transport_event_to_state(
            ldk_runtime,
            use_runtime_state_for_ln_views,
            peers,
            channels,
            &event,
        );
        record_runtime_event(
            runtime_events,
            RlnWasmNodeRuntimeEventData {
                seq,
                source: source.to_string(),
                event_kind: event.event_kind().to_string(),
                payload_hex,
                payment_hash: None,
                status: None,
                applied,
                error: if applied {
                    None
                } else {
                    Some("transport event target not found".to_string())
                },
                received_at,
            },
        );
        return Ok(());
    }

    record_runtime_event(
        runtime_events,
        RlnWasmNodeRuntimeEventData {
            seq,
            source: source.to_string(),
            event_kind: classify_non_payment_payload_kind(&payload_hex),
            payload_hex,
            payment_hash: None,
            status: None,
            applied: false,
            error: Some("unrecognized event payload format for payment status update".to_string()),
            received_at,
        },
    );
    Ok(())
}

fn drain_pending_peer_hook_events(
    ldk_runtime: &Rc<dyn LdkRuntimeManager>,
    use_runtime_state_for_ln_views: bool,
    peers: &Rc<RefCell<HashMap<String, PeerEntry>>>,
    channels: &Rc<RefCell<HashMap<String, ChannelEntry>>>,
    payments: &Rc<RefCell<HashMap<String, PaymentEntry>>>,
    pending_peer_hook_events: &Rc<RefCell<Vec<PendingPeerHookEvent>>>,
    runtime_events: &Rc<RefCell<Vec<RlnWasmNodeRuntimeEventData>>>,
    next_runtime_event_seq_ref: &Rc<RefCell<u64>>,
    source: &str,
) -> Result<usize, JsValue> {
    let drained = std::mem::take(&mut *pending_peer_hook_events.borrow_mut());
    let mut applied = 0usize;
    for pending_event in drained {
        match pending_event {
            PendingPeerHookEvent::Payload(payload_hex) => {
                apply_runtime_hook_payload(
                    ldk_runtime,
                    use_runtime_state_for_ln_views,
                    peers,
                    channels,
                    payments,
                    runtime_events,
                    next_runtime_event_seq_ref,
                    payload_hex,
                    source,
                )?;
                applied += 1;
            }
            PendingPeerHookEvent::SocketDisconnected => {
                fail_pending_payments_for_hook(
                    ldk_runtime,
                    use_runtime_state_for_ln_views,
                    peers,
                    channels,
                    payments,
                    runtime_events,
                    next_runtime_event_seq_ref,
                    source,
                )?;
                record_runtime_control_event(
                    runtime_events,
                    next_runtime_event_seq_ref,
                    "peer_hook_disconnected",
                    "".to_string(),
                    Some("peer manager socket disconnected".to_string()),
                );
                applied += 1;
            }
            PendingPeerHookEvent::Error(message) => {
                fail_pending_payments_for_hook(
                    ldk_runtime,
                    use_runtime_state_for_ln_views,
                    peers,
                    channels,
                    payments,
                    runtime_events,
                    next_runtime_event_seq_ref,
                    source,
                )?;
                record_runtime_control_event(
                    runtime_events,
                    next_runtime_event_seq_ref,
                    "peer_hook_error",
                    hex::encode(message.as_bytes()),
                    Some(message),
                );
                applied += 1;
            }
        }
    }
    Ok(applied)
}

fn fail_pending_payments_for_hook(
    ldk_runtime: &Rc<dyn LdkRuntimeManager>,
    use_runtime_state_for_ln_views: bool,
    peers: &Rc<RefCell<HashMap<String, PeerEntry>>>,
    channels: &Rc<RefCell<HashMap<String, ChannelEntry>>>,
    payments: &Rc<RefCell<HashMap<String, PaymentEntry>>>,
    runtime_events: &Rc<RefCell<Vec<RlnWasmNodeRuntimeEventData>>>,
    next_runtime_event_seq_ref: &Rc<RefCell<u64>>,
    source: &str,
) -> Result<usize, JsValue> {
    if use_runtime_state_for_ln_views {
        let pending_hashes = ldk_runtime
            .list_payments()
            .into_iter()
            .filter(|p| p.status == "pending")
            .map(|p| p.payment_hash)
            .collect::<Vec<_>>();
        let mut applied = 0usize;
        for payment_hash in pending_hashes {
            let payload_hex = encode_payment_status_event_payload(&payment_hash, "failed");
            apply_runtime_hook_payload(
                ldk_runtime,
                use_runtime_state_for_ln_views,
                peers,
                channels,
                payments,
                runtime_events,
                next_runtime_event_seq_ref,
                payload_hex,
                source,
            )?;
            applied += 1;
        }
        Ok(applied)
    } else {
        fail_pending_payments_with_runtime_events(
            payments,
            runtime_events,
            next_runtime_event_seq_ref,
            source,
            "failed",
        )
    }
}

fn apply_transport_event_to_state(
    ldk_runtime: &Rc<dyn LdkRuntimeManager>,
    use_runtime_state_for_ln_views: bool,
    peers: &Rc<RefCell<HashMap<String, PeerEntry>>>,
    channels: &Rc<RefCell<HashMap<String, ChannelEntry>>>,
    event: &RuntimeTransportEvent,
) -> bool {
    if use_runtime_state_for_ln_views {
        return match event {
            RuntimeTransportEvent::PeerDisconnected { peer_pubkey } => {
                let removed_peer = ldk_runtime.remove_peer(peer_pubkey);
                let removed_channels = ldk_runtime.remove_channels_by_peer(peer_pubkey) > 0;
                removed_peer || removed_channels
            }
            RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
                ldk_runtime.set_peer_started(peer_pubkey, true)
            }
            RuntimeTransportEvent::ChannelClosed { channel_id } => {
                ldk_runtime.remove_channel(channel_id)
            }
            RuntimeTransportEvent::ChannelUsable { channel_id } => {
                ldk_runtime.set_channel_usable(channel_id, true)
            }
            RuntimeTransportEvent::ChannelUnusable { channel_id } => {
                ldk_runtime.set_channel_usable(channel_id, false)
            }
        };
    }

    match event {
        RuntimeTransportEvent::PeerDisconnected { peer_pubkey } => {
            let removed_peer = peers.borrow_mut().remove(peer_pubkey).is_some();
            let mut guard = channels.borrow_mut();
            let before = guard.len();
            guard.retain(|_, ch| ch.data.peer_pubkey != *peer_pubkey);
            removed_peer || guard.len() != before
        }
        RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
            peers.borrow().contains_key(peer_pubkey)
        }
        RuntimeTransportEvent::ChannelClosed { channel_id } => {
            channels.borrow_mut().remove(channel_id).is_some()
        }
        RuntimeTransportEvent::ChannelUsable { channel_id } => {
            let mut guard = channels.borrow_mut();
            let Some(channel) = guard.get_mut(channel_id) else {
                return false;
            };
            channel.data.is_usable = true;
            channel.data.ready = true;
            channel.data.status = "opened".to_string();
            true
        }
        RuntimeTransportEvent::ChannelUnusable { channel_id } => {
            let mut guard = channels.borrow_mut();
            let Some(channel) = guard.get_mut(channel_id) else {
                return false;
            };
            channel.data.is_usable = false;
            channel.data.ready = false;
            channel.data.status = "pending".to_string();
            true
        }
    }
}

fn classify_non_payment_payload_kind(payload_hex: &str) -> String {
    let Ok(bytes) = hex::decode(payload_hex) else {
        return "invalid_hex_payload".to_string();
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return "binary_payload".to_string();
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "empty_payload".to_string();
    }
    if trimmed.starts_with('{') {
        return "json_payload".to_string();
    }
    if trimmed.contains(':') {
        return "text_protocol_payload".to_string();
    }
    "text_payload".to_string()
}

fn parse_transport_event_payload(payload_hex: &str) -> Option<RuntimeTransportEvent> {
    let bytes = hex::decode(payload_hex).ok()?;
    let text = std::str::from_utf8(&bytes).ok()?.trim();
    if text.is_empty() {
        return None;
    }

    if let Ok(value) = serde_json::from_str::<RuntimeTransportEvent>(text) {
        return Some(value);
    }
    if let Some(value) = parse_transport_event_payload_json_alias(text) {
        return Some(value);
    }

    let mut parts = text.split(':');
    let kind = parts.next()?;
    let kind = normalize_transport_event_kind(kind);
    let id = parts.next()?.trim().to_string();
    if id.is_empty() || parts.next().is_some() {
        return None;
    }
    match kind.as_str() {
        "peer_disconnected" => Some(RuntimeTransportEvent::PeerDisconnected { peer_pubkey: id }),
        "peer_offline" => Some(RuntimeTransportEvent::PeerDisconnected { peer_pubkey: id }),
        "peer_down" => Some(RuntimeTransportEvent::PeerDisconnected { peer_pubkey: id }),
        "peer_reconnected" => Some(RuntimeTransportEvent::PeerReconnected { peer_pubkey: id }),
        "peer_connected" => Some(RuntimeTransportEvent::PeerReconnected { peer_pubkey: id }),
        "peer_online" => Some(RuntimeTransportEvent::PeerReconnected { peer_pubkey: id }),
        "peer_up" => Some(RuntimeTransportEvent::PeerReconnected { peer_pubkey: id }),
        "channel_closed" => Some(RuntimeTransportEvent::ChannelClosed { channel_id: id }),
        "channel_usable" => Some(RuntimeTransportEvent::ChannelUsable { channel_id: id }),
        "channel_opened" => Some(RuntimeTransportEvent::ChannelUsable { channel_id: id }),
        "channel_ready" => Some(RuntimeTransportEvent::ChannelUsable { channel_id: id }),
        "channel_online" => Some(RuntimeTransportEvent::ChannelUsable { channel_id: id }),
        "channel_up" => Some(RuntimeTransportEvent::ChannelUsable { channel_id: id }),
        "channel_unusable" => Some(RuntimeTransportEvent::ChannelUnusable { channel_id: id }),
        "channel_disconnected" => Some(RuntimeTransportEvent::ChannelUnusable { channel_id: id }),
        "channel_offline" => Some(RuntimeTransportEvent::ChannelUnusable { channel_id: id }),
        "channel_down" => Some(RuntimeTransportEvent::ChannelUnusable { channel_id: id }),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct RuntimeTransportEventAliasPayload {
    #[serde(default)]
    event: Option<String>,
    #[serde(default, rename = "event_name")]
    event_name: Option<String>,
    #[serde(default, rename = "eventName")]
    event_name_camel: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default, rename = "type")]
    event_type: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    peer_pubkey: Option<String>,
    #[serde(default, rename = "peerPubkey")]
    peer_pubkey_camel: Option<String>,
    #[serde(default, rename = "peer_id")]
    peer_id: Option<String>,
    #[serde(default, rename = "peerId")]
    peer_id_camel: Option<String>,
    #[serde(default, rename = "node_id")]
    node_id: Option<String>,
    #[serde(default, rename = "nodeId")]
    node_id_camel: Option<String>,
    #[serde(default)]
    channel_id: Option<String>,
    #[serde(default, rename = "channelId")]
    channel_id_camel: Option<String>,
}

fn parse_transport_event_payload_json_alias(text: &str) -> Option<RuntimeTransportEvent> {
    let payload = serde_json::from_str::<RuntimeTransportEventAliasPayload>(text).ok()?;
    let event = payload
        .event
        .or(payload.event_name)
        .or(payload.event_name_camel)
        .or(payload.kind)
        .or(payload.event_type)
        .map(|event| normalize_transport_event_kind(&event))?;
    if event.is_empty() {
        return None;
    }
    let peer_id = payload
        .peer_pubkey
        .or(payload.peer_pubkey_camel)
        .or(payload.peer_id)
        .or(payload.peer_id_camel)
        .or(payload.node_id)
        .or(payload.node_id_camel)
        .or(payload.id.clone())
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty());
    let channel_id = payload
        .channel_id
        .or(payload.channel_id_camel)
        .or(payload.id.clone())
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty());
    match event.as_str() {
        "peer_disconnected" | "peerdisconnected" | "peer_offline" | "peeroffline" | "peer_down"
        | "peerdown" => {
            let peer_pubkey = peer_id.clone()?;
            Some(RuntimeTransportEvent::PeerDisconnected { peer_pubkey })
        }
        "peer_reconnected" | "peerreconnected" | "peer_connected" | "peerconnected"
        | "peer_online" | "peeronline" | "peer_up" | "peerup" => {
            let peer_pubkey = peer_id?;
            Some(RuntimeTransportEvent::PeerReconnected { peer_pubkey })
        }
        "channel_closed" | "channelclosed" => {
            let channel_id = channel_id.clone()?;
            Some(RuntimeTransportEvent::ChannelClosed { channel_id })
        }
        "channel_usable" | "channelusable" => {
            let channel_id = channel_id.clone()?;
            Some(RuntimeTransportEvent::ChannelUsable { channel_id })
        }
        "channel_opened" | "channelopened" | "channel_ready" | "channelready"
        | "channel_online" | "channelonline" | "channel_up" | "channelup" => {
            let channel_id = channel_id.clone()?;
            Some(RuntimeTransportEvent::ChannelUsable { channel_id })
        }
        "channel_unusable" | "channelunusable" | "channel_offline" | "channeloffline"
        | "channel_down" | "channeldown" => {
            let channel_id = channel_id.clone()?;
            Some(RuntimeTransportEvent::ChannelUnusable { channel_id })
        }
        "channel_disconnected" | "channeldisconnected" => {
            let channel_id = channel_id?;
            Some(RuntimeTransportEvent::ChannelUnusable { channel_id })
        }
        _ => None,
    }
}

fn normalize_transport_event_kind(kind: &str) -> String {
    kind.trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .replace('.', "_")
        .replace(' ', "_")
}

fn encode_transport_event_payload(event: &RuntimeTransportEvent) -> String {
    let payload = match event {
        RuntimeTransportEvent::PeerDisconnected { peer_pubkey } => {
            format!("peer_disconnected:{peer_pubkey}")
        }
        RuntimeTransportEvent::PeerReconnected { peer_pubkey } => {
            format!("peer_reconnected:{peer_pubkey}")
        }
        RuntimeTransportEvent::ChannelClosed { channel_id } => {
            format!("channel_closed:{channel_id}")
        }
        RuntimeTransportEvent::ChannelUsable { channel_id } => {
            format!("channel_usable:{channel_id}")
        }
        RuntimeTransportEvent::ChannelUnusable { channel_id } => {
            format!("channel_unusable:{channel_id}")
        }
    };
    hex::encode(payload.as_bytes())
}

fn encode_payment_status_event_payload(payment_hash: &str, status: &str) -> String {
    hex::encode(format!("payment_status:{payment_hash}:{status}").as_bytes())
}

fn validate_peer_addr_format(peer_addr: &str) -> Result<(), JsValue> {
    let trimmed = peer_addr.trim();
    let Some((host, port)) = trimmed.rsplit_once(':') else {
        return Err(JsValue::from_str("peer_addr must be in host:port format"));
    };
    if host.trim().is_empty() || port.trim().is_empty() {
        return Err(JsValue::from_str("peer_addr must be in host:port format"));
    }
    if !port.chars().all(|c| c.is_ascii_digit()) {
        return Err(JsValue::from_str("peer_addr port must be numeric"));
    }
    let port_num = port
        .parse::<u16>()
        .map_err(|_| JsValue::from_str("peer_addr port must be in range 0..=65535"))?;
    let _ = port_num;
    Ok(())
}

fn validate_asset_id_format(asset_id: &str) -> Result<(), JsValue> {
    let trimmed = asset_id.trim();
    if trimmed.len() != 64 || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(JsValue::from_str("invalid asset_id"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;

fn parse_payment_status_event_payload(payload_hex: &str) -> Option<PaymentStatusEvent> {
    let bytes = hex::decode(payload_hex).ok()?;
    let text = std::str::from_utf8(&bytes).ok()?.trim();
    if text.is_empty() {
        return None;
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
        if let Some(mapped) = parse_payment_status_event_json(&value) {
            return Some(mapped);
        }
    }

    let mut parts = text.split(':');
    let kind = parts.next()?;
    let kind = normalize_payment_event_kind(kind);
    match kind.as_str() {
        "payment_status" => {
            let payment_hash = parts.next()?;
            let status = parts.next()?;
            if parts.next().is_some() {
                return None;
            }
            Some(PaymentStatusEvent {
                payment_hash: payment_hash.to_string(),
                status: status.to_string(),
            })
        }
        "payment_succeeded" | "payment_failed" | "payment_expired" => {
            let payment_hash = parts.next()?;
            if parts.next().is_some() {
                return None;
            }
            Some(PaymentStatusEvent {
                payment_hash: payment_hash.to_string(),
                status: payment_status_from_event_kind(&kind)?.to_string(),
            })
        }
        _ => None,
    }
}

fn parse_payment_status_event_json(value: &serde_json::Value) -> Option<PaymentStatusEvent> {
    let payment_hash = value
        .get("payment_hash")
        .and_then(|v| v.as_str())
        .or_else(|| value.get("paymentHash").and_then(|v| v.as_str()))
        .or_else(|| value.get("hash").and_then(|v| v.as_str()))
        .or_else(|| value.get("payment_id").and_then(|v| v.as_str()))
        .or_else(|| value.get("paymentId").and_then(|v| v.as_str()))?
        .trim()
        .to_string();
    if payment_hash.is_empty() {
        return None;
    }

    if let Some(status) = value
        .get("status")
        .and_then(|v| v.as_str())
        .or_else(|| value.get("state").and_then(|v| v.as_str()))
        .or_else(|| value.get("payment_status").and_then(|v| v.as_str()))
        .or_else(|| value.get("paymentStatus").and_then(|v| v.as_str()))
    {
        let normalized = status.trim();
        if normalized.is_empty() {
            return None;
        }
        let mapped = normalize_payment_status(normalized)
            .ok()
            .or_else(|| payment_status_from_event_kind(normalized).map(ToString::to_string))
            .unwrap_or_else(|| normalized.to_string());
        return Some(PaymentStatusEvent {
            payment_hash,
            status: mapped,
        });
    }

    let event_kind = value
        .get("event")
        .and_then(|v| v.as_str())
        .or_else(|| value.get("event_name").and_then(|v| v.as_str()))
        .or_else(|| value.get("eventName").and_then(|v| v.as_str()))
        .or_else(|| value.get("kind").and_then(|v| v.as_str()))
        .or_else(|| value.get("type").and_then(|v| v.as_str()))?
        .trim();
    let status = payment_status_from_event_kind(event_kind)?;
    Some(PaymentStatusEvent {
        payment_hash,
        status: status.to_string(),
    })
}

fn payment_status_from_event_kind(kind: &str) -> Option<&'static str> {
    match normalize_payment_event_kind(kind).as_str() {
        "payment_succeeded" | "paymentsent" | "payment_sent" | "paymentclaimed"
        | "payment_claimed" | "payment_success" | "payment_completed" | "paymentcomplete" => {
            Some("succeeded")
        }
        "payment_failed" | "paymentfailed" | "payment_fail" | "payment_error" | "paymenterror" => {
            Some("failed")
        }
        "payment_expired" | "paymentexpired" | "payment_timeout" | "payment_timed_out"
        | "paymenttimedout" => Some("expired"),
        _ => None,
    }
}

fn normalize_payment_event_kind(kind: &str) -> String {
    kind.trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .replace('.', "_")
        .replace(' ', "_")
}
