use thiserror::Error;

pub const ERR_PROXY_URL_EMPTY: &str = "proxy_url cannot be empty";
pub const ERR_MNEMONIC_EMPTY: &str = "mnemonic cannot be empty";
pub const ERR_MNEMONIC_EMPTY_WHEN_PROVIDED: &str = "mnemonic cannot be empty when provided";
pub const ERR_PASSWORD_EMPTY: &str = "password cannot be empty";
pub const ERR_INDEXER_URL_EMPTY: &str = "indexer_url cannot be empty";
pub const ERR_ADDRESS_EMPTY: &str = "address cannot be empty";
pub const ERR_ASSET_ID_EMPTY: &str = "asset_id cannot be empty";
pub const ERR_ASSET_ID_EMPTY_IF_PROVIDED: &str = "asset_id cannot be empty if provided";
pub const ERR_INVOICE_STRING_EMPTY: &str = "invoice_string cannot be empty";
pub const ERR_SERVER_URL_EMPTY: &str = "server_url cannot be empty";
pub const ERR_STORE_ID_EMPTY: &str = "store_id cannot be empty";
pub const ERR_RELAY_AUTH_TOKEN_NODE_ID_TOGETHER: &str =
    "relay_auth_token and relay_node_id must be provided together";
pub const ERR_RELAY_AUTH_TOKEN_EMPTY: &str = "relay_auth_token cannot be empty";
pub const ERR_RELAY_NODE_ID_EMPTY: &str = "relay_node_id cannot be empty";
pub const ERR_RELAY_NODE_ID_INVALID: &str = "invalid relay_node_id";
pub const ERR_PEER_PUBKEY_INVALID: &str = "invalid peer_pubkey";
pub const ERR_RGB_PROXY_ENDPOINT_EMPTY: &str = "rgb_proxy_endpoint cannot be empty";
pub const ERR_RGB_PROXY_ENDPOINT_SCHEME: &str = "rgb_proxy_endpoint must use http:// or https://";
pub const ERR_RGB_PROXY_AUTH_TOKEN_NODE_ID_TOGETHER: &str =
    "rgb_proxy_auth_token and rgb_proxy_node_id must be provided together";
pub const ERR_RGB_PROXY_AUTH_TOKEN_EMPTY: &str = "rgb_proxy_auth_token cannot be empty";
pub const ERR_RGB_PROXY_NODE_ID_EMPTY: &str = "rgb_proxy_node_id cannot be empty";
pub const ERR_RGB_PROXY_NODE_ID_INVALID: &str = "invalid rgb_proxy_node_id";
pub const ERR_TRANSPORT_ENDPOINTS_MISSING: &str =
    "transport_endpoints must be provided or setRgbProxyTransport must be configured";
pub const ERR_APP_ID_EMPTY: &str = "app_id cannot be empty";
pub const ERR_RECIPIENT_GROUPS_EMPTY: &str = "recipient_groups cannot be empty";

// WASM SDK lifecycle / initialization errors (stable strings)
pub const ERR_SDK_NOT_INITIALIZED: &str = "sdk is not initialized";
pub const ERR_SDK_LIFECYCLE_INCONSISTENT: &str = "sdk lifecycle state is inconsistent";
pub const ERR_INVALID_PASSWORD: &str = "invalid password";
pub const ERR_ONLINE_ID_INVALID: &str =
    "Invalid Online object: id must be a u64 string or safe integer";

// WASM wallet / media / encoding validation (stable strings)
pub const ERR_UNSIGNED_PSBT_EMPTY: &str = "unsigned_psbt cannot be empty";
pub const ERR_MEDIA_DIGEST_INVALID: &str = "invalid media digest";
pub const ERR_MIME_EMPTY: &str = "mime cannot be empty";
pub const ERR_BYTES_HEX_EMPTY: &str = "bytes_hex cannot be empty";
pub const ERR_BYTES_HEX_INVALID: &str = "bytes_hex must be valid hex";
pub const ERR_MEDIA_FILE_EMPTY: &str = "media file cannot be empty";

// WASM networking / peer address parsing (stable strings)
pub const ERR_PEER_ADDR_EMPTY: &str = "peer_addr cannot be empty";
pub const ERR_PEER_ADDR_HOST_PORT: &str = "peer_addr must be in host:port format";
pub const ERR_PEER_ADDR_PORT_NUMERIC: &str = "peer_addr port must be numeric";
pub const ERR_PEER_ADDR_HOST_EMPTY: &str = "peer_addr host cannot be empty";

// Misc runtime keys (stable strings)
pub const ERR_RUNTIME_SCOPE_KEY_EMPTY: &str = "runtime_scope_key cannot be empty";
pub const ERR_SESSION_KEY_EMPTY: &str = "session_key cannot be empty";
pub const ERR_PAYLOAD_HEX_EMPTY: &str = "payload_hex cannot be empty";

// WASM LN transport / replay envelope (stable strings)
pub const ERR_ON_MESSAGE_CALLBACK_MISSING: &str = "missing on_message callback";
pub const ERR_REPLAY_ENVELOPE_FRAME_INVALID: &str = "invalid replay envelope frame";
pub const ERR_REPLAY_ENVELOPE_PAYLOAD_HEX_INVALID: &str = "invalid replay envelope payload_hex";
pub const ERR_RECONNECT_DELAYS_INVALID: &str = "reconnect delays must be > 0";
pub const ERR_MAX_RECONNECT_ATTEMPTS_TOO_LARGE: &str = "max_reconnect_attempts is too large";
pub const ERR_REPLAY_SESSION_ID_EMPTY: &str = "replay_session_id cannot be empty";

// WASM LDK live backend (stable strings)
pub const ERR_LDK_OBJECT_GRAPH_NOT_INITIALIZED: &str = "ldk object graph is not initialized";
pub const ERR_PEER_TRANSPORT_DISCONNECTED: &str = "peer transport is disconnected";
pub const ERR_ACTIVE_PEER_DESCRIPTOR_MISSING: &str = "no active peer descriptor";
pub const ERR_PEER_MANAGER_NEW_OUTBOUND_FAILED: &str = "peer_manager.new_outbound_connection failed";
pub const ERR_PEER_MANAGER_READ_EVENT_FAILED: &str = "peer_manager.read_event failed";
pub const ERR_OUTBOUND_QUEUE_LOCK_POISONED: &str = "outbound queue lock poisoned";
pub const ERR_RUNTIME_KEY_EMPTY: &str = "runtime_key cannot be empty";
pub const ERR_CAPACITY_SAT_ZERO: &str = "capacity_sat must be > 0";

// WASM swap runtime validation (stable strings)
pub const ERR_ASSET_ID_INVALID: &str = "invalid asset_id";
pub const ERR_SWAP_BTC_FOR_BTC: &str = "cannot swap BTC for BTC";
pub const ERR_SWAP_SAME_ASSET: &str = "cannot swap the same asset";
pub const ERR_SWAPSTRING_EMPTY: &str = "swapstring cannot be empty";
pub const ERR_SWAPSTRING_FORMAT_INVALID: &str = "invalid swapstring format";
pub const ERR_PAYMENT_HASH_EMPTY: &str = "payment_hash cannot be empty";
pub const ERR_PAYMENT_HASH_INVALID: &str = "invalid payment_hash";
pub const ERR_PAYMENT_SECRET_EMPTY: &str = "payment_secret cannot be empty";
pub const ERR_PAYMENT_SECRET_INVALID: &str = "invalid payment_secret";
pub const ERR_TAKER_PUBKEY_EMPTY: &str = "taker_pubkey cannot be empty";
pub const ERR_TAKER_PUBKEY_INVALID: &str = "invalid taker_pubkey";
pub const ERR_SWAP_QTY_FROM_ZERO: &str = "qty_from must be greater than 0";
pub const ERR_SWAP_QTY_TO_ZERO: &str = "qty_to must be greater than 0";
pub const ERR_SWAP_TIMEOUT_ZERO: &str = "timeout_sec must be greater than 0";
pub const ERR_SWAP_NOT_FOUND: &str = "swap not found";
pub const ERR_SWAP_EXPIRED: &str = "swap is expired";
pub const ERR_SWAP_REFERENCE_EMPTY: &str = "swap reference cannot be empty";

// WASM peer session / LDK hooks (stable strings)
pub const ERR_NEW_OUTBOUND_CONNECTION_CB_HEX_STRING: &str =
    "new_outbound_connection callback must return a hex string";
pub const ERR_PEER_PUBKEY_EMPTY: &str = "peer_pubkey cannot be empty";

// WASM node (sdk) validation (stable strings)
pub const ERR_WALLET_NOT_ATTACHED: &str = "wallet is not attached to node";
pub const ERR_AMOUNTS_EMPTY: &str = "amounts cannot be empty";
pub const ERR_TICKER_EMPTY: &str = "ticker cannot be empty";
pub const ERR_NAME_EMPTY: &str = "name cannot be empty";
pub const ERR_PEER_NOT_CONNECTED: &str = "peer is not connected";
pub const ERR_INVOICE_EMPTY: &str = "invoice cannot be empty";
pub const ERR_ASSET_AMOUNT_NONPOSITIVE: &str = "asset_amount must be > 0 when provided";
pub const ERR_AMT_MSAT_NONPOSITIVE: &str = "amt_msat must be > 0 when provided";
pub const ERR_PAYMENT_NOT_FOUND_AFTER_CREATION: &str = "payment not found after creation";
pub const ERR_DEST_PUBKEY_EMPTY: &str = "dest_pubkey cannot be empty";
pub const ERR_DEST_PUBKEY_INVALID: &str = "invalid dest_pubkey";
pub const ERR_PAYMENT_NOT_FOUND_AFTER_KEYSEND: &str = "payment not found after keysend";
pub const ERR_PAYMENT_HASH_ALREADY_USED: &str = "payment_hash already used";
pub const ERR_PAYMENT_NOT_FOUND_AFTER_INVOICE_CREATION: &str = "payment not found after invoice creation";
pub const ERR_LN_INVOICE_UNKNOWN: &str = "unknown LN invoice";
pub const ERR_LN_INVOICE_NOT_HODL: &str = "invoice is not hodl";
pub const ERR_LN_INVOICE_ALREADY_CLAIMED: &str = "invoice is already claimed";
pub const ERR_LN_INVOICE_SETTLING: &str = "invoice settling is in progress";
pub const ERR_LN_INVOICE_NOT_CLAIMABLE: &str = "invoice is not claimable";
pub const ERR_PAYMENT_PREIMAGE_EMPTY: &str = "payment_preimage cannot be empty";
pub const ERR_PAYMENT_PREIMAGE_INVALID: &str = "invalid payment_preimage";
pub const ERR_EXPIRY_SEC_NONPOSITIVE: &str = "expiry_sec must be > 0";
pub const ERR_PEER_ADDR_PORT_RANGE: &str = "peer_addr port must be in range 0..=65535";
pub const ERR_PAYMENT_NOT_FOUND: &str = "payment not found";

// WASM node/channel/runtime misc (stable strings)
pub const ERR_NODE_PUBKEY_DERIVE_FAILED: &str = "failed to derive local node pubkey";
pub const ERR_CHANNEL_ID_EMPTY: &str = "channel_id cannot be empty";
pub const ERR_CHANNEL_NOT_FOUND_AFTER_OPEN: &str = "channel not found after open";
pub const ERR_CHANNEL_NOT_FOUND: &str = "channel not found";
pub const ERR_TEMPORARY_CHANNEL_ID_EMPTY: &str = "temporary_channel_id cannot be empty";
pub const ERR_TEMPORARY_CHANNEL_ID_UNKNOWN: &str = "unknown temporary channel ID";
pub const ERR_CHAIN_SYNC_WASM32_ONLY: &str = "chain sync is only available in wasm32 runtime";
pub const ERR_NODE_RUNTIME_ID_EMPTY: &str = "node_runtime_id cannot be empty";
pub const ERR_VIRTUAL_OPEN_MODE_EMPTY: &str = "virtual_open_mode cannot be empty";
pub const ERR_VIRTUAL_CHANNELS_PUBLIC_FALSE: &str = "virtual channels requires public=false";
pub const ERR_VIRTUAL_CLEANUP_IN_PROGRESS: &str = "virtual cleanup is already in progress";
pub const ERR_NODE_IDENTITY_DERIVE_FAILED: &str = "failed to derive local node identity";

#[derive(Debug, Clone, Error)]
pub enum SdkError {
    #[error("{0}")]
    Message(String),
}

impl SdkError {
    pub fn msg(msg: impl Into<String>) -> Self {
        Self::Message(msg.into())
    }
}
