use std::collections::HashMap;
use std::collections::VecDeque;
use std::env;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::connect_info::ConnectInfo;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::DefaultBodyLimit;
use axum::extract::{Path, Query};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
#[cfg(feature = "dev-http")]
use axum::http::Method;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Extension;
use axum::{Json, Router};
#[cfg(feature = "dev-http")]
use bitcoin::{Address, Network, ScriptBuf, Transaction};
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

// Default environment values (single source of truth).
const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:3001";
const DEFAULT_RGB_UPSTREAM: &str = "http://127.0.0.1:3000/json-rpc";
#[cfg(feature = "dev-http")]
const DEFAULT_REGULAR_RLN_API_BASE: &str = "http://127.0.0.1:3101";

const DEFAULT_CORS_ALLOW_ORIGINS: &str = "http://127.0.0.1:8080,http://localhost:8080";

// In non-dev builds, default to requiring relay auth to avoid exposing
// a TCP relay usable by any browser client on allowed origins.
#[cfg(feature = "dev-http")]
const DEFAULT_RELAY_AUTH_REQUIRED: bool = false;
#[cfg(not(feature = "dev-http"))]
const DEFAULT_RELAY_AUTH_REQUIRED: bool = true;

const DEFAULT_RGB_REQUEST_TIMEOUT_MS: u64 = 15_000;
const DEFAULT_WS_MAX_FRAME_BYTES: usize = 128 * 1024;
const DEFAULT_WS_MAX_MESSAGE_BYTES: usize = 256 * 1024;
const DEFAULT_TCP_CONNECT_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_IO_IDLE_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_TCP_READ_CHUNK_BYTES: usize = 8192;
const DEFAULT_RGB_MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

const DEFAULT_ALLOW_PUBLIC_TARGETS: bool = false;
const DEFAULT_TARGET_ALLOWLIST: &str = "127.0.0.1,localhost,::1";

const DEFAULT_MAX_ACTIVE_WS: usize = 512;
const DEFAULT_MAX_ACTIVE_WS_PER_IP: usize = 64;

#[cfg(feature = "dev-http")]
const DEFAULT_REGTEST_FUNDING_ENABLED: bool = true;
#[cfg(feature = "dev-http")]
const DEFAULT_BITCOIN_RPC_URL: &str = "http://127.0.0.1:19443";
#[cfg(feature = "dev-http")]
// Matches `bindings/wasm-sdk/compose.wasm-infra.yaml` (hashbeam bitcoind RPCAUTH user/pass).
const DEFAULT_BITCOIN_RPC_USER: &str = "user";
#[cfg(feature = "dev-http")]
const DEFAULT_BITCOIN_RPC_PASSWORD: &str = "password";
#[cfg(feature = "dev-http")]
const DEFAULT_BITCOIN_RPC_WALLET: &str = "bdk-test";
#[cfg(feature = "dev-http")]
const DEFAULT_REGTEST_FUNDING_USE_BITCOIN_RPC: bool = true;

const DEFAULT_REPLAY_MAX_FRAMES: usize = 1024;
const DEFAULT_REPLAY_TTL_SECS: u64 = 300;
const DEFAULT_REPLAY_MAX_SESSIONS: usize = 256;

#[derive(Clone)]
struct RelayAuthConfig {
    required: bool,
    expected_token: Option<String>,
    expected_node_id: Option<String>,
}

#[derive(Clone)]
struct AppState {
    http: Client,
    rgb_upstream: String,
    #[cfg(feature = "dev-http")]
    regular_rln_api_base: String,
    relay_auth: RelayAuthConfig,
    runtime: RelayRuntimeConfig,
    target_policy: RelayTargetPolicy,
    relay_limiter: RelayLimiter,
    #[cfg(feature = "dev-http")]
    funding: FundingConfig,
    replay_buffers: Arc<Mutex<HashMap<String, ReplaySessionBuffer>>>,
    replay_max_frames: usize,
    replay_ttl_secs: u64,
    replay_max_sessions: usize,
}

#[derive(Clone)]
#[cfg(feature = "dev-http")]
struct FundingConfig {
    enabled: bool,
    bitcoin_rpc_base_url: String,
    bitcoin_rpc_user: String,
    bitcoin_rpc_password: String,
    bitcoin_rpc_wallet: String,
    use_bitcoin_rpc: bool,
}

#[derive(Debug, Deserialize)]
struct RelayQuery {
    auth_token: Option<String>,
    node_id: Option<String>,
    replay: Option<u8>,
    session_id: Option<String>,
    last_applied_seq: Option<u64>,
}

#[derive(Clone)]
struct ReplaySessionBuffer {
    next_seq: u64,
    frames: VecDeque<ReplayFrame>,
    last_touched_unix: u64,
}

#[derive(Clone)]
struct ReplayFrame {
    seq: u64,
    payload_hex: String,
    queued_at_unix: u64,
}

#[derive(Clone)]
struct ReplayRelayConfig {
    enabled: bool,
    session_id: Option<String>,
    last_applied_seq: Option<u64>,
    buffers: Arc<Mutex<HashMap<String, ReplaySessionBuffer>>>,
    max_frames: usize,
    ttl_secs: u64,
    max_sessions: usize,
}

fn is_valid_replay_session_id(value: &str) -> bool {
    if value.is_empty() || value.len() > 64 {
        return false;
    }
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

fn enforce_replay_session_cap(
    state: &mut HashMap<String, ReplaySessionBuffer>,
    replay_max_frames: usize,
    replay_ttl_secs: u64,
    max_sessions: usize,
) {
    let max_sessions = max_sessions.max(1);
    if state.len() < max_sessions {
        return;
    }
    // Evict oldest sessions by `last_touched_unix` first; also apply TTL on frames.
    let now = unix_now_secs();
    let mut entries: Vec<(String, u64)> = state
        .iter()
        .map(|(k, v)| (k.clone(), v.last_touched_unix))
        .collect();
    entries.sort_by_key(|(_, touched)| *touched);

    // Remove enough oldest sessions to make room for at least one new session.
    let to_remove = state.len().saturating_sub(max_sessions - 1).max(1);
    for (key, _) in entries.into_iter().take(to_remove) {
        state.remove(&key);
    }

    // Best-effort TTL cleanup for remaining sessions to avoid retaining old data indefinitely.
    for buffer in state.values_mut() {
        let before = buffer.frames.len();
        buffer
            .frames
            .retain(|frame| now.saturating_sub(frame.queued_at_unix) <= replay_ttl_secs);
        let _ = before;
        // Overflow guard (in case env changed).
        while buffer.frames.len() > replay_max_frames.max(1) {
            let _ = buffer.frames.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_session_id_validation() {
        assert!(!is_valid_replay_session_id(""));
        assert!(!is_valid_replay_session_id(" "));
        assert!(!is_valid_replay_session_id("a/b"));
        assert!(!is_valid_replay_session_id("a.b"));
        assert!(is_valid_replay_session_id("abcDEF012_-"));
        assert!(!is_valid_replay_session_id(&"a".repeat(65)));
        assert!(is_valid_replay_session_id(&"a".repeat(64)));
    }

    #[test]
    fn replay_session_cap_eviction() {
        let mut map: HashMap<String, ReplaySessionBuffer> = HashMap::new();
        for i in 0..10 {
            map.insert(
                format!("s{i}"),
                ReplaySessionBuffer {
                    next_seq: 1,
                    frames: VecDeque::new(),
                    last_touched_unix: i,
                },
            );
        }

        enforce_replay_session_cap(&mut map, 1024, 300, 4);
        assert!(map.len() <= 4);
        // Oldest (s0...) should be evicted first.
        assert!(!map.contains_key("s0"));
        assert!(!map.contains_key("s1"));
    }

    #[test]
    fn relay_limiter_releases_permit_on_drop() {
        let limiter = RelayLimiter::new(1, 1);
        let ip: IpAddr = "127.0.0.1".parse().unwrap();

        let permit = limiter.try_acquire(ip).expect("first acquire ok");
        assert!(limiter.try_acquire(ip).is_err(), "second acquire blocked");
        drop(permit);
        assert!(limiter.try_acquire(ip).is_ok(), "released after drop");
    }
}

#[cfg(feature = "dev-http")]
#[derive(Debug, Deserialize)]
struct RegtestFundRequest {
    address: String,
    amount_btc: Option<f64>,
    mine_blocks: Option<u16>,
}

#[cfg(feature = "dev-http")]
#[derive(Debug, Deserialize)]
struct RegtestFundingTxRequest {
    output_script_hex: String,
    amount_sat: u64,
    mine_blocks: Option<u16>,
}

#[cfg(feature = "dev-http")]
#[derive(Deserialize)]
struct RegtestBroadcastTxRequest {
    tx_hex: String,
    mine_blocks: Option<u16>,
}

#[derive(Clone)]
struct RelayRuntimeConfig {
    rgb_request_timeout_ms: u64,
    ws_max_frame_bytes: usize,
    ws_max_message_bytes: usize,
    tcp_connect_timeout_ms: u64,
    io_idle_timeout_ms: u64,
    tcp_read_chunk_bytes: usize,
}

#[derive(Clone)]
struct RelayTargetPolicy {
    allow_public_targets: bool,
    allowlist_exact: Vec<String>,
}

#[derive(Default)]
struct RelayLimiterState {
    active_global: usize,
    active_by_ip: HashMap<IpAddr, usize>,
}

#[derive(Clone)]
struct RelayLimiter {
    max_global: usize,
    max_per_ip: usize,
    state: Arc<StdMutex<RelayLimiterState>>,
}

struct RelayPermit {
    limiter: RelayLimiter,
    ip: IpAddr,
}

impl Drop for RelayPermit {
    fn drop(&mut self) {
        // Must not leak counts under contention; use a blocking mutex.
        let Ok(mut state) = self.limiter.state.lock() else {
            return;
        };
        state.active_global = state.active_global.saturating_sub(1);
        if let Some(ip_count) = state.active_by_ip.get_mut(&self.ip) {
            *ip_count = ip_count.saturating_sub(1);
            if *ip_count == 0 {
                state.active_by_ip.remove(&self.ip);
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let listen_addr = env_or("WASM_PROXY_LISTEN_ADDR", DEFAULT_LISTEN_ADDR);
    let addr: SocketAddr = listen_addr
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid WASM_PROXY_LISTEN_ADDR '{}': {e}", listen_addr))?;

    let rgb_upstream = env_or("WASM_PROXY_RGB_UPSTREAM", DEFAULT_RGB_UPSTREAM);
    #[cfg(feature = "dev-http")]
    let regular_rln_api_base = env_or(
        "WASM_PROXY_REGULAR_RLN_API_BASE",
        DEFAULT_REGULAR_RLN_API_BASE,
    );
    let relay_auth = RelayAuthConfig {
        required: env_bool(
            "WASM_PROXY_RELAY_AUTH_REQUIRED",
            DEFAULT_RELAY_AUTH_REQUIRED,
        ),
        expected_token: env::var("WASM_PROXY_RELAY_AUTH_TOKEN")
            .ok()
            .and_then(non_empty_trimmed),
        expected_node_id: env::var("WASM_PROXY_RELAY_NODE_ID")
            .ok()
            .and_then(non_empty_trimmed),
    };
    if relay_auth.required {
        if relay_auth.expected_token.is_none() || relay_auth.expected_node_id.is_none() {
            anyhow::bail!(
                "relay auth is required but WASM_PROXY_RELAY_AUTH_TOKEN / WASM_PROXY_RELAY_NODE_ID are not both set"
            );
        }
        info!("relay auth is enabled");
    }
    let runtime = RelayRuntimeConfig {
        rgb_request_timeout_ms: env_u64(
            "WASM_PROXY_RGB_REQUEST_TIMEOUT_MS",
            DEFAULT_RGB_REQUEST_TIMEOUT_MS,
        ),
        ws_max_frame_bytes: env_usize("WASM_PROXY_WS_MAX_FRAME_BYTES", DEFAULT_WS_MAX_FRAME_BYTES),
        ws_max_message_bytes: env_usize(
            "WASM_PROXY_WS_MAX_MESSAGE_BYTES",
            DEFAULT_WS_MAX_MESSAGE_BYTES,
        ),
        tcp_connect_timeout_ms: env_u64(
            "WASM_PROXY_TCP_CONNECT_TIMEOUT_MS",
            DEFAULT_TCP_CONNECT_TIMEOUT_MS,
        ),
        io_idle_timeout_ms: env_u64("WASM_PROXY_IO_IDLE_TIMEOUT_MS", DEFAULT_IO_IDLE_TIMEOUT_MS),
        tcp_read_chunk_bytes: env_usize(
            "WASM_PROXY_TCP_READ_CHUNK_BYTES",
            DEFAULT_TCP_READ_CHUNK_BYTES,
        ),
    };
    let rgb_max_body_bytes = env_usize("WASM_PROXY_RGB_MAX_BODY_BYTES", DEFAULT_RGB_MAX_BODY_BYTES);
    let cors_allow_origins = env_csv("WASM_PROXY_CORS_ALLOW_ORIGINS", DEFAULT_CORS_ALLOW_ORIGINS);
    let target_policy = RelayTargetPolicy {
        allow_public_targets: env_bool(
            "WASM_PROXY_ALLOW_PUBLIC_TARGETS",
            DEFAULT_ALLOW_PUBLIC_TARGETS,
        ),
        allowlist_exact: env_csv("WASM_PROXY_TARGET_ALLOWLIST", DEFAULT_TARGET_ALLOWLIST),
    };
    let relay_limiter = RelayLimiter::new(
        env_usize("WASM_PROXY_MAX_ACTIVE_WS", DEFAULT_MAX_ACTIVE_WS),
        env_usize(
            "WASM_PROXY_MAX_ACTIVE_WS_PER_IP",
            DEFAULT_MAX_ACTIVE_WS_PER_IP,
        ),
    );
    #[cfg(feature = "dev-http")]
    let funding = FundingConfig {
        enabled: env_bool(
            "WASM_PROXY_REGTEST_FUNDING_ENABLED",
            DEFAULT_REGTEST_FUNDING_ENABLED,
        ),
        bitcoin_rpc_base_url: env_or("WASM_PROXY_BITCOIN_RPC_URL", DEFAULT_BITCOIN_RPC_URL),
        bitcoin_rpc_user: env_or("WASM_PROXY_BITCOIN_RPC_USER", DEFAULT_BITCOIN_RPC_USER),
        bitcoin_rpc_password: env_or(
            "WASM_PROXY_BITCOIN_RPC_PASSWORD",
            DEFAULT_BITCOIN_RPC_PASSWORD,
        ),
        bitcoin_rpc_wallet: env_or("WASM_PROXY_BITCOIN_RPC_WALLET", DEFAULT_BITCOIN_RPC_WALLET),
        use_bitcoin_rpc: env_bool(
            "WASM_PROXY_REGTEST_FUNDING_USE_BITCOIN_RPC",
            DEFAULT_REGTEST_FUNDING_USE_BITCOIN_RPC,
        ),
    };
    let replay_max_frames =
        env_usize("WASM_PROXY_REPLAY_MAX_FRAMES", DEFAULT_REPLAY_MAX_FRAMES).max(64);
    let replay_ttl_secs = env_u64("WASM_PROXY_REPLAY_TTL_SECS", DEFAULT_REPLAY_TTL_SECS).max(30);
    let replay_max_sessions = env_usize(
        "WASM_PROXY_REPLAY_MAX_SESSIONS",
        DEFAULT_REPLAY_MAX_SESSIONS,
    )
    .max(8);
    #[cfg(feature = "dev-http")]
    if funding.enabled {
        info!(
            bitcoin_rpc_base_url = %funding.bitcoin_rpc_base_url,
            wallet = %funding.bitcoin_rpc_wallet,
            "regtest funding helper endpoint enabled"
        );
    }

    let state = Arc::new(AppState {
        http: Client::new(),
        rgb_upstream,
        #[cfg(feature = "dev-http")]
        regular_rln_api_base,
        relay_auth,
        runtime,
        target_policy,
        relay_limiter,
        #[cfg(feature = "dev-http")]
        funding,
        replay_buffers: Arc::new(Mutex::new(HashMap::new())),
        replay_max_frames,
        replay_ttl_secs,
        replay_max_sessions,
    });

    let cors_allow_origins_set = cors_allow_origins.clone();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |origin: &HeaderValue, _req| {
            let Ok(origin_str) = origin.to_str() else {
                return false;
            };
            cors_allow_origins_set
                .iter()
                .any(|allowed| allowed == origin_str)
        }))
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([CONTENT_TYPE, AUTHORIZATION]);

    // Important: apply layers *after* all routes are registered.
    // In axum, adding routes after `.layer(...)` may bypass those layers.
    // `mut` is only needed when the `dev-http` routes below are compiled in.
    #[cfg_attr(not(feature = "dev-http"), allow(unused_mut))]
    let mut app = Router::new()
        .route("/healthz", get(healthz))
        .route("/rgb/json-rpc", post(rgb_json_rpc))
        .route("/v1/:host/:port", get(ln_ws_relay))
        .route("/ln/v1/:host/:port", get(ln_ws_relay));

    #[cfg(feature = "dev-http")]
    {
        app = app
            .route("/dev/regular-rln/*path", get(regular_rln_proxy_get))
            .route("/dev/regular-rln/*path", post(regular_rln_proxy_post))
            .route("/dev/regtest/fund", post(regtest_fund))
            .route("/dev/regtest/broadcast-tx", post(regtest_broadcast_tx))
            .route("/dev/regtest/funding-tx", post(regtest_funding_tx));
    }

    let app = app
        .layer(DefaultBodyLimit::max(rgb_max_body_bytes))
        .layer(cors)
        .layer(Extension(state));

    run_gateway(addr, app).await
}

async fn run_gateway(addr: SocketAddr, app: Router) -> anyhow::Result<()> {
    info!(listen = %addr, "starting wasm proxy gateway");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,wasm_proxy_gateway=debug"));
    tracing_subscriber::fmt().with_env_filter(env_filter).init();
}

fn env_or(key: &str, default_value: &str) -> String {
    env::var(key)
        .ok()
        .and_then(non_empty_trimmed)
        .unwrap_or_else(|| default_value.to_string())
}

fn env_bool(key: &str, default_value: bool) -> bool {
    let Some(raw) = env::var(key).ok().and_then(non_empty_trimmed) else {
        return default_value;
    };
    matches!(
        raw.to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "y" | "on"
    )
}

fn env_u64(key: &str, default_value: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(non_empty_trimmed)
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(default_value)
}

fn env_usize(key: &str, default_value: usize) -> usize {
    env::var(key)
        .ok()
        .and_then(non_empty_trimmed)
        .and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(default_value)
}

fn env_csv(key: &str, default_value: &str) -> Vec<String> {
    env::var(key)
        .ok()
        .and_then(non_empty_trimmed)
        .unwrap_or_else(|| default_value.to_string())
        .split(',')
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
        .collect()
}

fn non_empty_trimmed(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

async fn healthz() -> impl IntoResponse {
    Json(json!({ "ok": true }))
}

#[cfg(feature = "dev-http")]
async fn regular_rln_proxy_get(
    Extension(state): Extension<Arc<AppState>>,
    Path(path): Path<String>,
) -> impl IntoResponse {
    regular_rln_proxy_impl(state, Method::GET, path, Bytes::new()).await
}

#[cfg(feature = "dev-http")]
async fn regular_rln_proxy_post(
    Extension(state): Extension<Arc<AppState>>,
    Path(path): Path<String>,
    body: Bytes,
) -> impl IntoResponse {
    regular_rln_proxy_impl(state, Method::POST, path, body).await
}

#[cfg(feature = "dev-http")]
async fn regular_rln_proxy_impl(
    state: Arc<AppState>,
    method: Method,
    path: String,
    body: Bytes,
) -> Response {
    let trimmed = path.trim_start_matches('/');
    let target_url = if trimmed.is_empty() {
        state.regular_rln_api_base.clone()
    } else {
        format!(
            "{}/{}",
            state.regular_rln_api_base.trim_end_matches('/'),
            trimmed
        )
    };

    let mut req = state.http.request(method, target_url.clone());
    if !body.is_empty() {
        req = req
            .header(CONTENT_TYPE, "application/json")
            .body(body.clone());
    }

    let response = match req.send().await {
        Ok(resp) => resp,
        Err(err) => {
            error!(%err, %target_url, "regular rln proxy request failed");
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": "regular_rln_proxy_failed",
                    "details": err.to_string(),
                    "target_url": target_url,
                })),
            )
                .into_response();
        }
    };

    let status = response.status();
    let upstream_ct = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body_bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(err) => {
            error!(%err, %target_url, "regular rln proxy body read failed");
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": "regular_rln_proxy_read_failed",
                    "details": err.to_string(),
                    "target_url": target_url,
                })),
            )
                .into_response();
        }
    };

    let mut resp_headers = HeaderMap::new();
    if let Some(ct) = upstream_ct {
        if let Ok(val) = HeaderValue::from_str(&ct) {
            resp_headers.insert(CONTENT_TYPE, val);
        }
    }
    (status, resp_headers, body_bytes).into_response()
}

async fn rgb_json_rpc(
    Extension(state): Extension<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StatusCode> {
    let mut req = state.http.post(&state.rgb_upstream).body(body.to_vec());
    req = req.timeout(Duration::from_millis(state.runtime.rgb_request_timeout_ms));

    if let Some(content_type) = headers.get(CONTENT_TYPE) {
        req = req.header(CONTENT_TYPE, content_type);
    } else {
        req = req.header(CONTENT_TYPE, "application/json");
    }
    if let Some(auth) = headers.get(AUTHORIZATION) {
        req = req.header(AUTHORIZATION, auth);
    }

    let upstream_resp = req.send().await.map_err(|err| {
        error!(?err, upstream = %state.rgb_upstream, "rgb upstream request failed");
        StatusCode::BAD_GATEWAY
    })?;

    let status = upstream_resp.status();
    let upstream_headers = upstream_resp.headers().clone();
    let payload = upstream_resp.bytes().await.map_err(|err| {
        error!(?err, "failed to read rgb upstream response body");
        StatusCode::BAD_GATEWAY
    })?;

    let mut out_headers = HeaderMap::new();
    if let Some(content_type) = upstream_headers.get(CONTENT_TYPE) {
        out_headers.insert(CONTENT_TYPE, content_type.clone());
    } else {
        out_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    }
    Ok((status, out_headers, payload).into_response())
}

#[cfg(feature = "dev-http")]
async fn regtest_fund(
    Extension(state): Extension<Arc<AppState>>,
    Json(payload): Json<RegtestFundRequest>,
) -> Result<Response, StatusCode> {
    if !state.funding.enabled {
        return Err(StatusCode::NOT_FOUND);
    }

    let address = payload.address.trim();
    if address.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let amount_btc = payload.amount_btc.unwrap_or(1.0);
    if !amount_btc.is_finite() || amount_btc <= 0.0 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let amount_str = format!("{amount_btc:.8}");

    let mine_blocks = payload.mine_blocks.unwrap_or(6).max(1);

    let mut funded_backends: Vec<String> = Vec::new();
    let mut txids: Vec<String> = Vec::new();

    if state.funding.use_bitcoin_rpc {
        match bitcoin_rpc_call_wallet(
            &state.http,
            &state.funding,
            "sendtoaddress",
            json!([address, amount_str]),
        )
        .await
        {
            Ok(result) => {
                if let Some(txid) = result.as_str().map(str::trim).filter(|v| !v.is_empty()) {
                    txids.push(txid.to_string());
                }

                match bitcoin_rpc_call_wallet(
                    &state.http,
                    &state.funding,
                    "getnewaddress",
                    json!([]),
                )
                .await
                {
                    Ok(result) => {
                        if let Some(mine_addr) =
                            result.as_str().map(str::trim).filter(|v| !v.is_empty())
                        {
                            match bitcoin_rpc_call_wallet(
                                &state.http,
                                &state.funding,
                                "generatetoaddress",
                                json!([mine_blocks, mine_addr]),
                            )
                            .await
                            {
                                Ok(_) => funded_backends.push("bitcoin_rpc".to_string()),
                                Err(err) => warn!(?err, "bitcoin rpc mine helper failed"),
                            }
                        }
                    }
                    Err(err) => warn!(?err, "bitcoin rpc getnewaddress helper failed"),
                }
            }
            Err(err) => warn!(?err, "bitcoin rpc sendtoaddress helper failed"),
        }
    }

    if funded_backends.is_empty() {
        return Err(StatusCode::BAD_GATEWAY);
    }

    Ok(Json(json!({
        "ok": true,
        "txids": txids,
        "amount_btc": amount_str,
        "mine_blocks": mine_blocks,
        "funded_backends": funded_backends,
    }))
    .into_response())
}

#[cfg(feature = "dev-http")]
async fn regtest_funding_tx(
    Extension(state): Extension<Arc<AppState>>,
    Json(payload): Json<RegtestFundingTxRequest>,
) -> Result<Response, StatusCode> {
    if !state.funding.enabled {
        return Err(StatusCode::NOT_FOUND);
    }
    if payload.amount_sat == 0 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let script_hex = payload.output_script_hex.trim();
    if script_hex.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let script_bytes = hex::decode(script_hex).map_err(|_| StatusCode::BAD_REQUEST)?;
    let script = ScriptBuf::from_bytes(script_bytes);
    let address =
        Address::from_script(&script, Network::Regtest).map_err(|_| StatusCode::BAD_REQUEST)?;
    let address = address.to_string();

    let amount_btc = (payload.amount_sat as f64) / 100_000_000.0;
    let amount_str = format!("{amount_btc:.8}");
    let mine_blocks = payload.mine_blocks.unwrap_or(0);

    if !state.funding.use_bitcoin_rpc {
        return Err(StatusCode::BAD_GATEWAY);
    }
    // A fresh `esplora` data volume often doesn't have the wallet created or funded yet.
    // Make this endpoint self-contained so E2E doesn't depend on prior manual steps.
    if let Err(err) =
        bitcoin_rpc_ensure_wallet_ready(&state.http, &state.funding, payload.amount_sat).await
    {
        error!(?err, "regtest funding-tx: wallet prep failed");
        return Err(StatusCode::BAD_GATEWAY);
    }
    let funding_tx_hex =
        match bitcoin_rpc_create_funding_tx_hex(&state.http, &state.funding, &address, &amount_str)
            .await
        {
            Ok(v) => v,
            Err(err) => {
                error!(?err, "regtest funding-tx: create funding tx failed");
                return Err(StatusCode::BAD_GATEWAY);
            }
        };
    let funding_tx: Transaction = bitcoin::consensus::deserialize(
        &hex::decode(funding_tx_hex.trim()).map_err(|_| StatusCode::BAD_GATEWAY)?,
    )
    .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let txid = funding_tx.txid().to_string();

    Ok(Json(json!({
        "ok": true,
        "txid": txid,
        "funding_tx_hex": funding_tx_hex,
        "address": address,
        "amount_sat": payload.amount_sat,
        "mine_blocks": mine_blocks,
    }))
    .into_response())
}

#[cfg(feature = "dev-http")]
async fn regtest_broadcast_tx(
    Extension(state): Extension<Arc<AppState>>,
    Json(payload): Json<RegtestBroadcastTxRequest>,
) -> Result<Response, StatusCode> {
    if !state.funding.enabled {
        return Err(StatusCode::NOT_FOUND);
    }
    if !state.funding.use_bitcoin_rpc {
        return Err(StatusCode::BAD_GATEWAY);
    }
    let tx_hex = payload.tx_hex.trim();
    if tx_hex.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mine_blocks = payload.mine_blocks.unwrap_or(0);

    let txid_value = bitcoin_rpc_call_root(
        &state.http,
        &state.funding,
        "sendrawtransaction",
        json!([tx_hex]),
    )
    .await
    .map_err(|err| {
        error!(?err, "regtest broadcast-tx: sendrawtransaction failed");
        StatusCode::BAD_GATEWAY
    })?;

    if mine_blocks > 0 {
        bitcoin_rpc_mine_blocks(&state.http, &state.funding, mine_blocks)
            .await
            .map_err(|err| {
                error!(?err, "regtest broadcast-tx: mining failed");
                StatusCode::BAD_GATEWAY
            })?;
    }

    // Ensure the tx is actually confirmed on-chain as observed by bitcoind.
    if mine_blocks > 0 {
        let txid_str = txid_value.as_str().unwrap_or("").to_string();
        for _ in 0..50 {
            let raw = bitcoin_rpc_call_root(
                &state.http,
                &state.funding,
                "getrawtransaction",
                json!([txid_str, true]),
            )
            .await;
            if let Ok(v) = raw {
                let confs = v.get("confirmations").and_then(|c| c.as_u64()).unwrap_or(0);
                if confs > 0 {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    Ok(Json(json!({
        "ok": true,
        "txid": txid_value,
        "mine_blocks": mine_blocks,
    }))
    .into_response())
}

#[cfg(feature = "dev-http")]
async fn bitcoin_rpc_ensure_wallet_ready(
    http: &Client,
    cfg: &FundingConfig,
    target_send_amount_sat: u64,
) -> Result<(), anyhow::Error> {
    // 1) Ensure wallet exists.
    let wallet_info = bitcoin_rpc_call_wallet(http, cfg, "getwalletinfo", json!([])).await;
    if let Err(err) = wallet_info {
        let stderr = err.to_string().to_lowercase();
        // On a fresh volume bitcoind returns "Requested wallet does not exist or is not loaded".
        if stderr.contains("does not exist") || stderr.contains("not loaded") {
            bitcoin_rpc_call_root(http, cfg, "createwallet", json!([cfg.bitcoin_rpc_wallet]))
                .await?;
        } else {
            anyhow::bail!("getwalletinfo failed: {err}");
        }
    }

    // 2) Ensure wallet has funds mature enough to spend.
    // Coinbase requires 100 confirmations; mine 110 blocks on-demand if balance is too low.
    let balance_value = bitcoin_rpc_call_wallet(http, cfg, "getbalance", json!([])).await?;
    let balance_btc = balance_value.as_f64().unwrap_or(0.0);
    let target_btc = (target_send_amount_sat as f64) / 100_000_000.0;
    // Leave some room for fees.
    if balance_btc + 1e-8 < (target_btc + 0.001) {
        // Mine enough blocks so coinbase is spendable and we have a cushion.
        bitcoin_rpc_mine_blocks(http, cfg, 110).await?;
    }
    Ok(())
}

#[cfg(feature = "dev-http")]
async fn bitcoin_rpc_create_funding_tx_hex(
    http: &Client,
    cfg: &FundingConfig,
    address: &str,
    amount_str: &str,
) -> Result<String, anyhow::Error> {
    // `sendtoaddress` enables anti-fee-sniping by setting a non-zero nLockTime and non-final
    // input sequences. LDK rejects such funding transactions. Build a funded tx via PSBT with
    // `locktime=0` and `replaceable=false` so inputs get final sequence numbers.

    // 1) Create an initial funded PSBT so the wallet selects UTXOs + fee.
    let outputs = serde_json::json!([{ address: amount_str }]);
    let created1 = bitcoin_rpc_call_wallet(
        http,
        cfg,
        "walletcreatefundedpsbt",
        json!([
            [],
            outputs,
            0,
            { "add_inputs": true, "replaceable": false },
            true
        ]),
    )
    .await?;
    let psbt1 = created1
        .get("psbt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("walletcreatefundedpsbt response missing psbt"))?;

    // Some Bitcoin Core builds still set non-final sequence numbers even with `replaceable=false`.
    // LDK rejects such funding transactions. Recreate the PSBT with explicit input sequences.
    let decoded = bitcoin_rpc_call_root(http, cfg, "decodepsbt", json!([psbt1])).await?;
    let vin = decoded
        .get("tx")
        .and_then(|tx| tx.get("vin"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("decodepsbt response missing tx.vin"))?;
    let mut inputs = Vec::with_capacity(vin.len());
    for inp in vin {
        let txid = inp
            .get("txid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("decodepsbt vin missing txid"))?;
        let vout = inp
            .get("vout")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("decodepsbt vin missing vout"))?;
        inputs.push(serde_json::json!({
            "txid": txid,
            "vout": vout,
            "sequence": 4294967295u64
        }));
    }

    let created2 = bitcoin_rpc_call_wallet(
        http,
        cfg,
        "walletcreatefundedpsbt",
        json!([
            serde_json::Value::Array(inputs),
            outputs,
            0,
            { "add_inputs": false, "replaceable": false },
            true
        ]),
    )
    .await?;
    let psbt = created2
        .get("psbt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("walletcreatefundedpsbt response missing psbt"))?;

    // 2) Let wallet sign.
    let processed =
        bitcoin_rpc_call_wallet(http, cfg, "walletprocesspsbt", json!([psbt, true])).await?;
    let signed = processed
        .get("psbt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("walletprocesspsbt response missing psbt"))?;

    // 3) Finalize.
    let finalized = bitcoin_rpc_call_root(http, cfg, "finalizepsbt", json!([signed, true])).await?;
    let hex = finalized
        .get("hex")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("finalizepsbt response missing hex"))?;
    Ok(hex.trim().to_string())
}

#[cfg(feature = "dev-http")]
async fn bitcoin_rpc_mine_blocks(
    http: &Client,
    cfg: &FundingConfig,
    mine_blocks: u16,
) -> Result<(), anyhow::Error> {
    let mine_addr = bitcoin_rpc_call_wallet(http, cfg, "getnewaddress", json!([])).await?;
    let mine_addr = mine_addr
        .as_str()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("getnewaddress returned empty address"))?;
    let _ = bitcoin_rpc_call_wallet(
        http,
        cfg,
        "generatetoaddress",
        json!([mine_blocks, mine_addr]),
    )
    .await?;
    Ok(())
}

// NOTE: we no longer need `gettransaction/getrawtransaction` because we return the tx hex directly
// from `finalizepsbt` in `bitcoin_rpc_create_funding_tx_hex`.

#[cfg(feature = "dev-http")]
fn bitcoin_rpc_root_url(cfg: &FundingConfig) -> String {
    cfg.bitcoin_rpc_base_url.trim_end_matches('/').to_string()
}

#[cfg(feature = "dev-http")]
fn bitcoin_rpc_wallet_url(cfg: &FundingConfig) -> String {
    format!(
        "{}/wallet/{}",
        bitcoin_rpc_root_url(cfg),
        cfg.bitcoin_rpc_wallet
    )
}

#[cfg(feature = "dev-http")]
async fn bitcoin_rpc_call(
    http: &Client,
    url: &str,
    cfg: &FundingConfig,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, anyhow::Error> {
    let response = http
        .post(url)
        .basic_auth(&cfg.bitcoin_rpc_user, Some(&cfg.bitcoin_rpc_password))
        .json(&json!({
            "jsonrpc": "1.0",
            "id": "wasm-proxy-gateway",
            "method": method,
            "params": params,
        }))
        .send()
        .await?;
    let status = response.status();
    let body: serde_json::Value = response.json().await?;
    if !status.is_success() {
        anyhow::bail!("bitcoin rpc {method} failed with HTTP {status}: {body}");
    }
    if !body
        .get("error")
        .unwrap_or(&serde_json::Value::Null)
        .is_null()
    {
        anyhow::bail!("bitcoin rpc {method} returned error: {}", body["error"]);
    }
    body.get("result")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("bitcoin rpc {method} response missing result"))
}

#[cfg(feature = "dev-http")]
async fn bitcoin_rpc_call_root(
    http: &Client,
    cfg: &FundingConfig,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, anyhow::Error> {
    let url = bitcoin_rpc_root_url(cfg);
    bitcoin_rpc_call(http, &url, cfg, method, params).await
}

#[cfg(feature = "dev-http")]
async fn bitcoin_rpc_call_wallet(
    http: &Client,
    cfg: &FundingConfig,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, anyhow::Error> {
    let url = bitcoin_rpc_wallet_url(cfg);
    bitcoin_rpc_call(http, &url, cfg, method, params).await
}

async fn ln_ws_relay(
    ws: WebSocketUpgrade,
    Path((host_slug, port)): Path<(String, u16)>,
    Query(query): Query<RelayQuery>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Response, StatusCode> {
    validate_relay_query(&state.relay_auth, &query)?;
    let target_host = decode_host_slug(&host_slug).ok_or(StatusCode::BAD_REQUEST)?;
    validate_target_host(&target_host, &state.target_policy)?;
    let permit = state
        .relay_limiter
        .try_acquire(remote.ip())
        .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    let target = format!("{target_host}:{port}");
    let runtime = state.runtime.clone();
    let replay = ReplayRelayConfig {
        enabled: query.replay.unwrap_or(0) == 1,
        session_id: query
            .session_id
            .as_ref()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty()),
        last_applied_seq: query.last_applied_seq,
        buffers: Arc::clone(&state.replay_buffers),
        max_frames: state.replay_max_frames,
        ttl_secs: state.replay_ttl_secs,
        max_sessions: state.replay_max_sessions,
    };

    Ok(ws
        .max_frame_size(runtime.ws_max_frame_bytes)
        .max_message_size(runtime.ws_max_message_bytes)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            if let Err(err) = relay_ws_to_tcp(
                socket,
                target.clone(),
                runtime,
                replay,
            )
            .await
            {
                warn!(?err, target = %target, remote = %remote, "ws relay session finished with error");
            }
        }))
}

fn validate_relay_query(cfg: &RelayAuthConfig, query: &RelayQuery) -> Result<(), StatusCode> {
    if !cfg.required {
        return Ok(());
    }

    let token = query
        .auth_token
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let node_id = query
        .node_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    if let Some(expected) = cfg.expected_token.as_deref() {
        if token != expected {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }
    if let Some(expected) = cfg.expected_node_id.as_deref() {
        if node_id != expected {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }
    Ok(())
}

fn decode_host_slug(slug: &str) -> Option<String> {
    let trimmed = slug.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return None;
    }

    let decoded = trimmed.replace('_', ".");
    if decoded.is_empty() {
        None
    } else {
        Some(decoded)
    }
}

fn validate_target_host(host: &str, policy: &RelayTargetPolicy) -> Result<(), StatusCode> {
    let normalized = host.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if policy
        .allowlist_exact
        .iter()
        .any(|allowed| allowed == &normalized)
    {
        return Ok(());
    }

    if let Ok(ip) = normalized.parse::<IpAddr>() {
        if ip.is_loopback() {
            return Ok(());
        }
        if let IpAddr::V4(v4) = ip {
            if v4.is_private() || v4.is_link_local() {
                return Ok(());
            }
        }
    }

    if policy.allow_public_targets {
        return Ok(());
    }
    Err(StatusCode::FORBIDDEN)
}

async fn relay_ws_to_tcp(
    socket: WebSocket,
    target: String,
    runtime: RelayRuntimeConfig,
    replay: ReplayRelayConfig,
) -> anyhow::Result<()> {
    let stream = tokio::time::timeout(
        Duration::from_millis(runtime.tcp_connect_timeout_ms),
        TcpStream::connect(&target),
    )
    .await
    .map_err(|_| anyhow::anyhow!("tcp connect timeout"))??;
    info!(target = %target, "relay connected");

    let (mut tcp_read, mut tcp_write) = stream.into_split();
    let (mut ws_tx, mut ws_rx) = socket.split();

    let (tcp_to_ws_tx, mut tcp_to_ws_rx) = mpsc::channel::<Message>(64);

    if replay.enabled {
        if let Some(session_id) = replay.session_id.as_ref() {
            let replay_frames = {
                let mut state = replay.buffers.lock().await;
                let session_id = session_id.trim();
                if !is_valid_replay_session_id(session_id) {
                    anyhow::bail!("invalid replay session_id");
                }
                enforce_replay_session_cap(
                    &mut state,
                    replay.max_frames,
                    replay.ttl_secs,
                    replay.max_sessions,
                );
                let entry =
                    state
                        .entry(session_id.to_string())
                        .or_insert_with(|| ReplaySessionBuffer {
                            next_seq: 1,
                            frames: VecDeque::new(),
                            last_touched_unix: unix_now_secs(),
                        });
                let now = unix_now_secs();
                entry.last_touched_unix = now;
                let before_ttl = entry.frames.len();
                entry
                    .frames
                    .retain(|frame| now.saturating_sub(frame.queued_at_unix) <= replay.ttl_secs);
                let ttl_dropped = before_ttl.saturating_sub(entry.frames.len());
                if ttl_dropped > 0 {
                    info!(
                        session_id = %session_id,
                        dropped = ttl_dropped,
                        ttl_secs = replay.ttl_secs,
                        "replay TTL eviction applied"
                    );
                }
                let last = replay.last_applied_seq.unwrap_or(0);
                entry
                    .frames
                    .iter()
                    .filter(|frame| frame.seq > last)
                    .cloned()
                    .collect::<Vec<_>>()
            };
            for frame in replay_frames {
                let payload = json!({
                    "seq": frame.seq,
                    "payload_hex": frame.payload_hex,
                });
                let _ = tcp_to_ws_tx.send(Message::Text(payload.to_string())).await;
            }
        }
    }

    let tcp_reader = tokio::spawn(async move {
        let mut buf = vec![0u8; runtime.tcp_read_chunk_bytes.max(1024)];
        loop {
            let n = tokio::time::timeout(
                Duration::from_millis(runtime.io_idle_timeout_ms),
                tcp_read.read(&mut buf),
            )
            .await
            .map_err(|_| anyhow::anyhow!("tcp read idle timeout"))??;
            if n == 0 {
                break;
            }
            let chunk = buf[..n].to_vec();
            let outbound = if replay.enabled {
                if let Some(session_id) = replay.session_id.as_ref() {
                    let frame = {
                        let mut state = replay.buffers.lock().await;
                        let session_id = session_id.trim();
                        if !is_valid_replay_session_id(session_id) {
                            anyhow::bail!("invalid replay session_id");
                        }
                        enforce_replay_session_cap(
                            &mut state,
                            replay.max_frames,
                            replay.ttl_secs,
                            replay.max_sessions,
                        );
                        let entry = state.entry(session_id.to_string()).or_insert_with(|| {
                            ReplaySessionBuffer {
                                next_seq: 1,
                                frames: VecDeque::new(),
                                last_touched_unix: unix_now_secs(),
                            }
                        });
                        let now = unix_now_secs();
                        entry.last_touched_unix = now;
                        let before_ttl = entry.frames.len();
                        entry.frames.retain(|frame| {
                            now.saturating_sub(frame.queued_at_unix) <= replay.ttl_secs
                        });
                        let ttl_dropped = before_ttl.saturating_sub(entry.frames.len());
                        if ttl_dropped > 0 {
                            info!(
                                session_id = %session_id,
                                dropped = ttl_dropped,
                                ttl_secs = replay.ttl_secs,
                                "replay TTL eviction applied"
                            );
                        }
                        let seq = entry.next_seq;
                        entry.next_seq = entry.next_seq.saturating_add(1);
                        let frame = ReplayFrame {
                            seq,
                            payload_hex: hex::encode(&chunk),
                            queued_at_unix: now,
                        };
                        entry.frames.push_back(frame.clone());
                        let mut overflow_dropped = 0usize;
                        while entry.frames.len() > replay.max_frames {
                            let _ = entry.frames.pop_front();
                            overflow_dropped = overflow_dropped.saturating_add(1);
                        }
                        if overflow_dropped > 0 {
                            warn!(
                                session_id = %session_id,
                                dropped = overflow_dropped,
                                max_frames = replay.max_frames,
                                "replay buffer overflow drop policy applied"
                            );
                        }
                        frame
                    };
                    let payload = json!({
                        "seq": frame.seq,
                        "payload_hex": frame.payload_hex,
                    });
                    Message::Text(payload.to_string())
                } else {
                    Message::Binary(chunk)
                }
            } else {
                Message::Binary(chunk)
            };
            if tcp_to_ws_tx.send(outbound).await.is_err() {
                break;
            }
        }
        Ok::<(), anyhow::Error>(())
    });

    let ws_writer = tokio::spawn(async move {
        while let Some(frame) = tcp_to_ws_rx.recv().await {
            ws_tx.send(frame).await?;
        }
        Ok::<(), anyhow::Error>(())
    });

    while let Some(msg) = ws_rx.next().await {
        match msg? {
            Message::Binary(bytes) => {
                debug!(target = %target, bytes = bytes.len(), "ws->tcp binary frame");
                tokio::time::timeout(
                    Duration::from_millis(runtime.io_idle_timeout_ms),
                    tcp_write.write_all(&bytes),
                )
                .await
                .map_err(|_| anyhow::anyhow!("tcp write idle timeout"))??;
            }
            Message::Text(text) => {
                let trimmed = text.trim();
                let payload =
                    if trimmed.chars().all(|c| c.is_ascii_hexdigit()) && trimmed.len() % 2 == 0 {
                        match hex::decode(trimmed) {
                            Ok(bytes) => bytes,
                            Err(_) => trimmed.as_bytes().to_vec(),
                        }
                    } else {
                        trimmed.as_bytes().to_vec()
                    };
                debug!(target = %target, text_len = text.len(), bytes = payload.len(), "ws->tcp text frame");
                tokio::time::timeout(
                    Duration::from_millis(runtime.io_idle_timeout_ms),
                    tcp_write.write_all(&payload),
                )
                .await
                .map_err(|_| anyhow::anyhow!("tcp write idle timeout"))??;
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }

    let _ = tcp_write.shutdown().await;
    let _ = tcp_reader.await;
    let _ = ws_writer.await;
    info!(target = %target, "relay disconnected");
    Ok(())
}

fn unix_now_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl RelayLimiter {
    fn new(max_global: usize, max_per_ip: usize) -> Self {
        Self {
            max_global: max_global.max(1),
            max_per_ip: max_per_ip.max(1),
            state: Arc::new(StdMutex::new(RelayLimiterState::default())),
        }
    }

    fn try_acquire(&self, ip: IpAddr) -> Result<RelayPermit, ()> {
        let mut state = self.state.lock().map_err(|_| ())?;
        if state.active_global >= self.max_global {
            return Err(());
        }
        let current_ip = state.active_by_ip.get(&ip).copied().unwrap_or(0);
        if current_ip >= self.max_per_ip {
            return Err(());
        }
        state.active_global += 1;
        state.active_by_ip.insert(ip, current_ip + 1);
        Ok(RelayPermit {
            limiter: self.clone(),
            ip,
        })
    }
}
