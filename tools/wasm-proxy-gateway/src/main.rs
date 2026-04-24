use std::collections::HashMap;
use std::env;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::connect_info::ConnectInfo;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::DefaultBodyLimit;
use axum::extract::{Path, Query, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};
use tower_http::cors::CorsLayer;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

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
    relay_auth: RelayAuthConfig,
    runtime: RelayRuntimeConfig,
    target_policy: RelayTargetPolicy,
    relay_limiter: RelayLimiter,
    regtest_funding: RegtestFundingConfig,
}

#[derive(Clone)]
struct RegtestFundingConfig {
    enabled: bool,
    script_path: String,
    compose_workdir: String,
    esplora_service: String,
    esplora_rpc_user: String,
    esplora_rpc_password: String,
    esplora_wallet: String,
    use_regtest_script: bool,
    use_esplora_rpc: bool,
}

#[derive(Debug, Deserialize)]
struct RelayQuery {
    auth_token: Option<String>,
    node_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RegtestFundRequest {
    address: String,
    amount_btc: Option<f64>,
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
    state: Arc<Mutex<RelayLimiterState>>,
}

struct RelayPermit {
    limiter: RelayLimiter,
    ip: IpAddr,
}

impl Drop for RelayPermit {
    fn drop(&mut self) {
        if let Ok(mut state) = self.limiter.state.try_lock() {
            state.active_global = state.active_global.saturating_sub(1);
            if let Some(ip_count) = state.active_by_ip.get_mut(&self.ip) {
                *ip_count = ip_count.saturating_sub(1);
                if *ip_count == 0 {
                    state.active_by_ip.remove(&self.ip);
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let listen_addr = env_or("WASM_PROXY_LISTEN_ADDR", "127.0.0.1:3001");
    let addr: SocketAddr = listen_addr
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid WASM_PROXY_LISTEN_ADDR '{}': {e}", listen_addr))?;

    let rgb_upstream = env_or("WASM_PROXY_RGB_UPSTREAM", "http://127.0.0.1:3000/json-rpc");
    let relay_auth = RelayAuthConfig {
        required: env_bool("WASM_PROXY_RELAY_AUTH_REQUIRED", false),
        expected_token: env::var("WASM_PROXY_RELAY_AUTH_TOKEN")
            .ok()
            .and_then(non_empty_trimmed),
        expected_node_id: env::var("WASM_PROXY_RELAY_NODE_ID")
            .ok()
            .and_then(non_empty_trimmed),
    };
    if relay_auth.required {
        info!("relay auth is enabled");
    }
    let runtime = RelayRuntimeConfig {
        rgb_request_timeout_ms: env_u64("WASM_PROXY_RGB_REQUEST_TIMEOUT_MS", 15_000),
        ws_max_frame_bytes: env_usize("WASM_PROXY_WS_MAX_FRAME_BYTES", 128 * 1024),
        ws_max_message_bytes: env_usize("WASM_PROXY_WS_MAX_MESSAGE_BYTES", 256 * 1024),
        tcp_connect_timeout_ms: env_u64("WASM_PROXY_TCP_CONNECT_TIMEOUT_MS", 5_000),
        io_idle_timeout_ms: env_u64("WASM_PROXY_IO_IDLE_TIMEOUT_MS", 60_000),
        tcp_read_chunk_bytes: env_usize("WASM_PROXY_TCP_READ_CHUNK_BYTES", 8192),
    };
    let rgb_max_body_bytes = env_usize("WASM_PROXY_RGB_MAX_BODY_BYTES", 2 * 1024 * 1024);
    let target_policy = RelayTargetPolicy {
        allow_public_targets: env_bool("WASM_PROXY_ALLOW_PUBLIC_TARGETS", false),
        allowlist_exact: env_csv("WASM_PROXY_TARGET_ALLOWLIST", "127.0.0.1,localhost,::1"),
    };
    let relay_limiter = RelayLimiter::new(
        env_usize("WASM_PROXY_MAX_ACTIVE_WS", 512),
        env_usize("WASM_PROXY_MAX_ACTIVE_WS_PER_IP", 64),
    );
    let regtest_funding = RegtestFundingConfig {
        enabled: env_bool("WASM_PROXY_REGTEST_FUNDING_ENABLED", true),
        script_path: env_or("WASM_PROXY_REGTEST_SCRIPT_PATH", "./regtest.sh"),
        compose_workdir: env_or("WASM_PROXY_REGTEST_COMPOSE_WORKDIR", "."),
        esplora_service: env_or("WASM_PROXY_REGTEST_ESPLORA_SERVICE", "esplora"),
        esplora_rpc_user: env_or("WASM_PROXY_REGTEST_ESPLORA_RPC_USER", "admin"),
        esplora_rpc_password: env_or("WASM_PROXY_REGTEST_ESPLORA_RPC_PASSWORD", "passw"),
        esplora_wallet: env_or("WASM_PROXY_REGTEST_ESPLORA_WALLET", "bdk-test"),
        use_regtest_script: env_bool("WASM_PROXY_REGTEST_FUNDING_USE_REGTEST_SH", true),
        use_esplora_rpc: env_bool("WASM_PROXY_REGTEST_FUNDING_USE_ESPLORA_RPC", true),
    };
    if regtest_funding.enabled {
        info!(
            script = %regtest_funding.script_path,
            "regtest funding helper endpoint enabled"
        );
    }

    let state = Arc::new(AppState {
        http: Client::new(),
        rgb_upstream,
        relay_auth,
        runtime,
        target_policy,
        relay_limiter,
        regtest_funding,
    });

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/rgb/json-rpc", post(rgb_json_rpc))
        .route("/dev/regtest/fund", post(regtest_fund))
        .route("/v1/:host/:port", get(ln_ws_relay))
        .route("/ln/v1/:host/:port", get(ln_ws_relay))
        .layer(DefaultBodyLimit::max(rgb_max_body_bytes))
        .layer(CorsLayer::permissive())
        .with_state(state);

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

async fn rgb_json_rpc(
    State(state): State<Arc<AppState>>,
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

async fn regtest_fund(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<RegtestFundRequest>,
) -> Result<Response, StatusCode> {
    if !state.regtest_funding.enabled {
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
    let mine_blocks_str = mine_blocks.to_string();

    let mut funded_backends: Vec<String> = Vec::new();
    let mut txids: Vec<String> = Vec::new();

    if state.regtest_funding.use_regtest_script {
        let send_output = Command::new(&state.regtest_funding.script_path)
            .arg("sendtoaddress")
            .arg(address)
            .arg(amount_str.clone())
            .output()
            .await
            .map_err(|err| {
                error!(?err, "failed to execute regtest sendtoaddress helper");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        if send_output.status.success() {
            let txid = String::from_utf8_lossy(&send_output.stdout)
                .trim()
                .to_string();
            if !txid.is_empty() {
                txids.push(txid);
            }

            let mine_output = Command::new(&state.regtest_funding.script_path)
                .arg("mine")
                .arg(mine_blocks_str.clone())
                .output()
                .await
                .map_err(|err| {
                    error!(?err, "failed to execute regtest mine helper");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
            if mine_output.status.success() {
                funded_backends.push("regtest_sh".to_string());
            } else {
                let stderr = String::from_utf8_lossy(&mine_output.stderr);
                warn!(stderr = %stderr, "regtest mine helper failed");
            }
        } else {
            let stderr = String::from_utf8_lossy(&send_output.stderr);
            warn!(stderr = %stderr, "regtest sendtoaddress helper failed");
        }
    }

    if state.regtest_funding.use_esplora_rpc {
        let base_args = [
            "compose",
            "exec",
            "-T",
            &state.regtest_funding.esplora_service,
            "/root/bitcoin-cli",
            "-regtest",
            &format!("-rpcuser={}", state.regtest_funding.esplora_rpc_user),
            &format!(
                "-rpcpassword={}",
                state.regtest_funding.esplora_rpc_password
            ),
            &format!("-rpcwallet={}", state.regtest_funding.esplora_wallet),
        ];

        let send_output = Command::new("docker")
            .current_dir(&state.regtest_funding.compose_workdir)
            .args(base_args)
            .arg("sendtoaddress")
            .arg(address)
            .arg(amount_str.clone())
            .output()
            .await
            .map_err(|err| {
                error!(?err, "failed to execute esplora sendtoaddress helper");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

        if send_output.status.success() {
            let txid = String::from_utf8_lossy(&send_output.stdout)
                .trim()
                .to_string();
            if !txid.is_empty() {
                txids.push(txid);
            }

            let mine_addr_output = Command::new("docker")
                .current_dir(&state.regtest_funding.compose_workdir)
                .args(base_args)
                .arg("getnewaddress")
                .output()
                .await
                .map_err(|err| {
                    error!(?err, "failed to fetch esplora mining address");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
            if mine_addr_output.status.success() {
                let mine_addr = String::from_utf8_lossy(&mine_addr_output.stdout)
                    .trim()
                    .to_string();
                if !mine_addr.is_empty() {
                    let mine_output = Command::new("docker")
                        .current_dir(&state.regtest_funding.compose_workdir)
                        .args(base_args)
                        .arg("generatetoaddress")
                        .arg(mine_blocks_str.clone())
                        .arg(mine_addr)
                        .output()
                        .await
                        .map_err(|err| {
                            error!(?err, "failed to execute esplora mine helper");
                            StatusCode::INTERNAL_SERVER_ERROR
                        })?;
                    if mine_output.status.success() {
                        funded_backends.push("esplora_rpc".to_string());
                    } else {
                        let stderr = String::from_utf8_lossy(&mine_output.stderr);
                        warn!(stderr = %stderr, "esplora mine helper failed");
                    }
                }
            } else {
                let stderr = String::from_utf8_lossy(&mine_addr_output.stderr);
                warn!(stderr = %stderr, "esplora getnewaddress helper failed");
            }
        } else {
            let stderr = String::from_utf8_lossy(&send_output.stderr);
            warn!(stderr = %stderr, "esplora sendtoaddress helper failed");
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

async fn ln_ws_relay(
    ws: WebSocketUpgrade,
    Path((host_slug, port)): Path<(String, u16)>,
    Query(query): Query<RelayQuery>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    State(state): State<Arc<AppState>>,
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

    Ok(ws
        .max_frame_size(runtime.ws_max_frame_bytes)
        .max_message_size(runtime.ws_max_message_bytes)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            if let Err(err) = relay_ws_to_tcp(socket, target.clone(), runtime).await {
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

    let (tcp_to_ws_tx, mut tcp_to_ws_rx) = mpsc::channel::<Vec<u8>>(64);

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
            if tcp_to_ws_tx.send(buf[..n].to_vec()).await.is_err() {
                break;
            }
        }
        Ok::<(), anyhow::Error>(())
    });

    let ws_writer = tokio::spawn(async move {
        while let Some(chunk) = tcp_to_ws_rx.recv().await {
            ws_tx.send(Message::Binary(chunk)).await?;
        }
        Ok::<(), anyhow::Error>(())
    });

    while let Some(msg) = ws_rx.next().await {
        match msg? {
            Message::Binary(bytes) => {
                tokio::time::timeout(
                    Duration::from_millis(runtime.io_idle_timeout_ms),
                    tcp_write.write_all(&bytes),
                )
                .await
                .map_err(|_| anyhow::anyhow!("tcp write idle timeout"))??;
            }
            Message::Text(text) => {
                tokio::time::timeout(
                    Duration::from_millis(runtime.io_idle_timeout_ms),
                    tcp_write.write_all(text.as_bytes()),
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

impl RelayLimiter {
    fn new(max_global: usize, max_per_ip: usize) -> Self {
        Self {
            max_global: max_global.max(1),
            max_per_ip: max_per_ip.max(1),
            state: Arc::new(Mutex::new(RelayLimiterState::default())),
        }
    }

    fn try_acquire(&self, ip: IpAddr) -> Result<RelayPermit, ()> {
        let mut state = self.state.try_lock().map_err(|_| ())?;
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
