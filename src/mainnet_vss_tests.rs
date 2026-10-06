//! Real VSS client traffic against a versioned in-memory protobuf server. This verifies client
//! integration and encrypted wallet recovery, not production server authentication or durability.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use rgb_lib::BitcoinNetwork;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use vss_client::prost::Message;
use vss_client::types::{
    ErrorCode, ErrorResponse, GetObjectRequest, GetObjectResponse, KeyValue,
    ListKeyVersionsRequest, ListKeyVersionsResponse, PutObjectRequest, PutObjectResponse,
};

use crate::args::UserArgs;
use crate::core_types::LdkChainSync;
use crate::error::APIError;
use crate::mainnet_startup_tests::Indexer;
use crate::utils::{start_daemon, AppState};
use crate::{routes, sdk};

const PASSWORD: &str = "mainnet-vss-test-password";
const MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const FENCE: &str = "__rln_instance__";

#[derive(Default)]
struct Store {
    values: BTreeMap<String, KeyValue>,
    version: i64,
}

#[derive(Clone, Debug)]
struct Request {
    method: String,
    store: String,
    keys: Vec<String>,
    deletes: Vec<String>,
    authenticated: bool,
}

struct BackupGate {
    store: String,
    entered: Semaphore,
    resume: Semaphore,
}

#[derive(Default)]
struct ServerState {
    stores: BTreeMap<String, Store>,
    requests: Vec<Request>,
    backup_gate: Option<Arc<BackupGate>>,
}

impl ServerState {
    // Validate the complete transaction before changing any row, including fence CAS.
    fn put(&mut self, request: PutObjectRequest) -> Result<(), ErrorCode> {
        let store = self.stores.entry(request.store_id).or_default();
        if request.global_version.is_some_and(|v| v != store.version) {
            return Err(ErrorCode::ConflictException);
        }
        let mut keys = HashSet::new();
        for value in request
            .transaction_items
            .iter()
            .chain(&request.delete_items)
        {
            if !keys.insert(&value.key) || value.version < -1 {
                return Err(ErrorCode::InvalidRequestException);
            }
        }
        for value in &request.transaction_items {
            let version = store.values.get(&value.key).map_or(0, |v| v.version);
            if value.version != -1 && value.version != version {
                return Err(ErrorCode::ConflictException);
            }
        }
        for value in &request.delete_items {
            if !store
                .values
                .get(&value.key)
                .is_some_and(|existing| value.version == -1 || value.version == existing.version)
            {
                return Err(ErrorCode::ConflictException);
            }
        }
        for mut value in request.transaction_items {
            value.version = if value.version == -1 {
                1
            } else {
                value.version + 1
            };
            store.values.insert(value.key.clone(), value);
        }
        for value in request.delete_items {
            store.values.remove(&value.key);
        }
        store.version += 1;
        Ok(())
    }
}

fn protobuf(status: StatusCode, message: impl Message) -> Response {
    (
        status,
        [("content-type", "application/octet-stream")],
        message.encode_to_vec(),
    )
        .into_response()
}

fn failure(code: ErrorCode) -> Response {
    let status = match code {
        ErrorCode::NoSuchKeyException => StatusCode::NOT_FOUND,
        ErrorCode::ConflictException => StatusCode::CONFLICT,
        _ => StatusCode::BAD_REQUEST,
    };
    protobuf(
        status,
        ErrorResponse {
            error_code: code as i32,
            message: "fixture".into(),
        },
    )
}

async fn handle(
    State(state): State<Arc<Mutex<ServerState>>>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Pause a selected real RGB backup before committing its protobuf transaction.
    let gate = if uri.path() == "/vss/putObjects" {
        let request = PutObjectRequest::decode(body.clone()).unwrap();
        let mut state = state.lock().unwrap();
        if state.backup_gate.as_ref().is_some_and(|gate| {
            request.store_id == gate.store
                && request
                    .transaction_items
                    .iter()
                    .any(|item| item.key == "backup/data")
        }) {
            state.backup_gate.take()
        } else {
            None
        }
    } else {
        None
    };
    if let Some(gate) = gate {
        gate.entered.add_permits(1);
        gate.resume.acquire().await.unwrap().forget();
    }
    let mut state = state.lock().unwrap();
    let authenticated = headers.contains_key("authorization");
    match uri.path() {
        "/vss/getObject" => {
            let request = GetObjectRequest::decode(body).unwrap();
            state.requests.push(Request {
                method: "get".into(),
                store: request.store_id.clone(),
                keys: vec![request.key.clone()],
                deletes: vec![],
                authenticated,
            });
            match state
                .stores
                .get(&request.store_id)
                .and_then(|s| s.values.get(&request.key))
            {
                Some(value) => protobuf(
                    StatusCode::OK,
                    GetObjectResponse {
                        value: Some(value.clone()),
                    },
                ),
                None => failure(ErrorCode::NoSuchKeyException),
            }
        }
        "/vss/putObjects" => {
            let request = PutObjectRequest::decode(body).unwrap();
            state.requests.push(Request {
                method: "put".into(),
                store: request.store_id.clone(),
                keys: request
                    .transaction_items
                    .iter()
                    .map(|v| v.key.clone())
                    .collect(),
                deletes: request.delete_items.iter().map(|v| v.key.clone()).collect(),
                authenticated,
            });
            match state.put(request) {
                Ok(()) => protobuf(StatusCode::OK, PutObjectResponse {}),
                Err(code) => failure(code),
            }
        }
        "/vss/listKeyVersions" => {
            let request = ListKeyVersionsRequest::decode(body).unwrap();
            state.requests.push(Request {
                method: "list".into(),
                store: request.store_id.clone(),
                keys: vec![],
                deletes: vec![],
                authenticated,
            });
            let offset: usize = request
                .page_token
                .as_deref()
                .unwrap_or("0")
                .parse()
                .unwrap();
            let store = state.stores.get(&request.store_id);
            // Deliberately small pages and reverse key order exercise complete inventory.
            // VSS does not promise lexical order or a full requested page.
            let values: Vec<_> = store
                .into_iter()
                .flat_map(|s| s.values.values().rev())
                .filter(|v| {
                    request
                        .key_prefix
                        .as_ref()
                        .is_none_or(|p| v.key.starts_with(p))
                })
                .collect();
            let page_size = request
                .page_size
                .filter(|size| *size > 0)
                .unwrap_or(2)
                .min(2) as usize;
            let key_versions = values
                .iter()
                .skip(offset)
                .take(page_size)
                .map(|v| KeyValue {
                    key: v.key.clone(),
                    version: v.version,
                    value: vec![],
                })
                .collect();
            let next = offset + page_size;
            protobuf(
                StatusCode::OK,
                ListKeyVersionsResponse {
                    key_versions,
                    next_page_token: (next < values.len()).then(|| next.to_string()),
                    global_version: request
                        .page_token
                        .is_none()
                        .then(|| store.map_or(0, |s| s.version)),
                },
            )
        }
        _ => panic!("unexpected VSS method: {}", uri.path()),
    }
}

struct Server {
    url: String,
    state: Arc<Mutex<ServerState>>,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/vss", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(ServerState::default()));
        let router = Router::new()
            .fallback(post(handle))
            .with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { url, state, task }
    }

    fn rows(&self, store: &str) -> BTreeMap<String, KeyValue> {
        self.state
            .lock()
            .unwrap()
            .stores
            .get(store)
            .map(|s| s.values.clone())
            .unwrap_or_default()
    }
}

struct Wallet {
    state: Arc<AppState>,
    _directory: tempfile::TempDir,
    indexer: Indexer,
    proxy: String,
    proxy_task: JoinHandle<()>,
}
impl Drop for Wallet {
    fn drop(&mut self) {
        self.proxy_task.abort();
    }
}
impl Wallet {
    async fn new(server: &Server) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let indexer = Indexer::new(bitcoin::Network::Bitcoin).await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = format!("rpc://{}/json-rpc", listener.local_addr().unwrap());
        let router = Router::new().route("/json-rpc", post(|Json(request): Json<Value>| async move {
            assert_eq!(request["method"], "server.info");
            Json(json!({"jsonrpc":"2.0", "id":request["id"], "result":{"protocol_version":"0.2", "version":"fixture", "uptime":1}}))
        }));
        let proxy_task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let state = start_daemon(&UserArgs {
            storage_dir_path: directory.path().into(),
            daemon_listening_port: 0,
            ldk_peer_listening_port: 0,
            network: BitcoinNetwork::Mainnet,
            max_media_upload_size_mb: 1,
            max_aggregated_media_size_per_channel_mb: 1,
            max_pending_consignments: 10,
            max_media_files_per_channel: 10,
            root_public_key: None,
            enable_virtual_channels_v0: false,
            virtual_peer_pubkeys: vec![],
            lsp_base_url: None,
            lsp_bearer_token: None,
            vss_url: Some(server.url.clone()),
            vss_allow_empty_restore: false,
            reuse_addresses: true,
            remote_signer_listen_addr: None,
            config: Default::default(),
        })
        .await
        .unwrap();
        sdk::init(state.clone(), PASSWORD.into(), Some(MNEMONIC.into()))
            .await
            .unwrap();
        Self {
            state,
            _directory: directory,
            indexer,
            proxy,
            proxy_task,
        }
    }

    fn request(&self) -> sdk::UnlockRequest {
        sdk::UnlockRequest {
            password: PASSWORD.into(),
            indexer_url: Some(self.indexer.url.clone()),
            eth_rpc_url: None,
            proxy_endpoint: Some(self.proxy.clone()),
            announce_addresses: vec![],
            announce_alias: None,
            gossip_rgs_server_url: None,
            ldk_chain_sync: {
                #[cfg(feature = "block-sync")]
                {
                    LdkChainSync::BlockSync {
                        bitcoind_rpc_username: "unused".into(),
                        bitcoind_rpc_password: "unused".into(),
                        bitcoind_rpc_host: "127.0.0.1".into(),
                        bitcoind_rpc_port: 1,
                    }
                }
                #[cfg(not(feature = "block-sync"))]
                {
                    LdkChainSync::TransactionSync {
                        indexer_url: "tcp://127.0.0.1:1".into(),
                    }
                }
            },
        }
    }

    async fn lock(&self) {
        let _ = routes::lock(State(self.state.clone())).await.unwrap();
        assert!(self.state.unlocked_app_state.lock().await.is_none());
        assert!(self.state.ldk_background_services.lock().unwrap().is_none());
    }
}

fn store_id() -> String {
    let mnemonic = rgb_lib::bdk_wallet::keys::bip39::Mnemonic::parse(MNEMONIC).unwrap();
    crate::ldk::derive_vss_identity(&mnemonic, bitcoin::Network::Bitcoin)
        .unwrap()
        .pubkey_hex
}

#[test]
fn versioned_fixture_rejects_conflicts_atomically() {
    let mut state = ServerState::default();
    let put = |items| PutObjectRequest {
        store_id: "s".into(),
        global_version: None,
        transaction_items: items,
        delete_items: vec![],
    };
    let value = |key: &str, version| KeyValue {
        key: key.into(),
        version,
        value: vec![1],
    };
    state.put(put(vec![value("fence", 0)])).unwrap();
    assert_eq!(
        state.put(put(vec![value("new", 0), value("fence", 0)])),
        Err(ErrorCode::ConflictException)
    );
    assert!(!state.stores["s"].values.contains_key("new"));
    assert_eq!(state.stores["s"].values["fence"].version, 1);
    state.put(put(vec![value("fence", 1)])).unwrap();
    assert_eq!(state.stores["s"].values["fence"].version, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mainnet_vss_wallet_backup_reopen_and_fresh_device_restore() {
    let server = Server::new().await;
    let wallet = Wallet::new(&server).await;
    let node_store = store_id();
    let rgb_store = format!("{node_store}_rgb");
    sdk::unlock(wallet.state.clone(), wallet.request())
        .await
        .unwrap();
    assert!(wallet
        .state
        .unlocked_app_state
        .lock()
        .await
        .as_ref()
        .unwrap()
        .lightning
        .is_none());
    assert!(wallet
        .state
        .ldk_background_services
        .lock()
        .unwrap()
        .is_none());
    let first_fence = server.rows(&node_store)[FENCE].clone();
    let initial = sdk::address(wallet.state.clone()).await.unwrap().address;
    let rotated = sdk::rotate_address(wallet.state.clone())
        .await
        .unwrap()
        .address;
    assert_ne!(initial, rotated);
    let version = sdk::vss_backup(wallet.state.clone()).await.unwrap();
    let backup = server.rows(&rgb_store);
    assert_eq!(backup["backup/data"].version, version);
    assert!(!backup["backup/data"].value.is_empty());
    let manifest: Value = serde_json::from_slice(&backup["backup/manifest"].value).unwrap();
    assert_eq!(manifest["encrypted"], true);
    assert!(backup.contains_key("backup/metadata"));

    let other = Wallet::new(&server).await;
    let conflict = sdk::unlock(other.state.clone(), other.request()).await;
    assert!(
        matches!(&conflict, Err(APIError::FailedVssInit(detail)) if detail.contains("owned by another")),
        "{conflict:?}"
    );
    assert_eq!(server.rows(&node_store)[FENCE], first_fence);
    assert!(other.state.unlocked_app_state.lock().await.is_none());

    wallet.lock().await;
    assert!(!server.rows(&node_store).contains_key(FENCE));
    sdk::unlock(wallet.state.clone(), wallet.request())
        .await
        .unwrap();
    assert_ne!(server.rows(&node_store)[FENCE].value, first_fence.value);
    assert_eq!(
        sdk::address(wallet.state.clone()).await.unwrap().address,
        rotated
    );
    wallet.lock().await;
    let request_start = server.state.lock().unwrap().requests.len();
    sdk::unlock(other.state.clone(), other.request())
        .await
        .unwrap();
    assert_eq!(
        sdk::address(other.state.clone()).await.unwrap().address,
        rotated
    );
    assert!(other
        .state
        .unlocked_app_state
        .lock()
        .await
        .as_ref()
        .unwrap()
        .lightning
        .is_none());
    other.lock().await;
    assert!(!server.rows(&node_store).contains_key(FENCE));
    let state = server.state.lock().unwrap();
    assert!(state.requests[request_start..]
        .iter()
        .any(|r| r.method == "get" && r.store == rgb_store && r.keys == ["backup/data"]));
    assert!(state.requests.iter().all(|r| r.authenticated));
    assert!(state.stores[&node_store]
        .values
        .keys()
        .all(|key| key.starts_with("rgb/wallet_config/")));
    assert!(state
        .requests
        .iter()
        .filter(|r| r.store == node_store)
        .all(|r| {
            r.keys
                .iter()
                .chain(&r.deletes)
                .all(|key| key == FENCE || key.starts_with("rgb/wallet_config/"))
        }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mainnet_vss_preserves_legacy_records_and_intents_while_wallet_backup_works() {
    use lightning::util::persist::KVStoreSync;
    let server = Server::new().await;
    let wallet = Wallet::new(&server).await;
    let node_store = store_id();
    let local = crate::kv_store::SeaOrmKvStore::from_connection(wallet.state.db());
    let legacy_keys = ["_/_/manager", "_/_/scorer", "monitors/_/remote_only"];
    {
        let mut state = server.state.lock().unwrap();
        let store = state.stores.entry(node_store.clone()).or_default();
        for key in legacy_keys {
            store.values.insert(
                key.into(),
                KeyValue {
                    key: key.into(),
                    version: 7,
                    value: b"remote historical bytes".to_vec(),
                },
            );
        }
    }
    local
        .write("", "", "manager", b"different local manager".to_vec())
        .unwrap();
    let pending = [
        ("_/_/manager", b"\x01queued manager replacement".as_slice()),
        ("_/_/scorer", &[0]),
        ("monitors/_/remote_only", b"malformed intent".as_slice()),
        ("invalid-key", &[255]),
    ];
    for (key, value) in pending {
        local.write("vss_pending", "", key, value.to_vec()).unwrap();
    }
    let before = server.rows(&node_store);
    let wrong_indexer = Indexer::new(bitcoin::Network::Regtest).await;
    let mut wrong_request = wallet.request();
    wrong_request.indexer_url = Some(wrong_indexer.url.clone());
    assert!(matches!(
        sdk::unlock(wallet.state.clone(), wrong_request).await,
        Err(APIError::InvalidIndexer(_))
    ));
    assert!(wallet.state.unlocked_app_state.lock().await.is_none());
    assert!(wallet
        .state
        .ldk_background_services
        .lock()
        .unwrap()
        .is_none());
    assert!(!*wallet.state.changing_state.lock().unwrap());
    let after_failed_unlock = server.rows(&node_store);
    assert!(!after_failed_unlock.contains_key(FENCE));
    for key in legacy_keys {
        assert_eq!(after_failed_unlock[key], before[key]);
    }
    for (key, value) in pending {
        assert_eq!(local.read("vss_pending", "", key).unwrap(), value);
    }
    assert_eq!(
        local.read("", "", "manager").unwrap(),
        b"different local manager"
    );
    for _ in 0..2 {
        sdk::unlock(wallet.state.clone(), wallet.request())
            .await
            .unwrap();
        let common = {
            let state = wallet.state.unlocked_app_state.lock().await;
            assert!(state.as_ref().unwrap().lightning.is_none());
            state.as_ref().unwrap().common.clone()
        };
        assert!(wallet
            .state
            .ldk_background_services
            .lock()
            .unwrap()
            .is_none());
        // Exercise explicit drain as well as automatic drains caused by config writes and stop.
        common.kv_store.drain_pending();
        assert!(sdk::address(wallet.state.clone())
            .await
            .unwrap()
            .address
            .starts_with("bc1"));
        sdk::vss_backup(wallet.state.clone()).await.unwrap();
        wallet.lock().await;
        let after = server.rows(&node_store);
        assert!(!after.contains_key(FENCE));
        for key in legacy_keys {
            assert_eq!(after[key], before[key]);
        }
        assert_eq!(
            local.read("", "", "manager").unwrap(),
            b"different local manager"
        );
        assert!(local.read("monitors", "", "remote_only").is_err());
        for (key, value) in pending {
            assert_eq!(local.read("vss_pending", "", key).unwrap(), value);
        }
    }
    assert!(!server.rows(&format!("{node_store}_rgb"))["backup/data"]
        .value
        .is_empty());
    let state = server.state.lock().unwrap();
    assert!(state
        .requests
        .iter()
        .filter(|r| r.store == node_store)
        .all(|r| {
            r.method != "list"
                && r.keys
                    .iter()
                    .chain(&r.deletes)
                    .all(|key| key == FENCE || key.starts_with("rgb/wallet_config/"))
        }));
}

/// The second stop has no session ownership while the first is still stopping its store.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mainnet_vss_concurrent_stop_keeps_fence_with_teardown_owner() {
    let server = Server::new().await;
    let wallet = Wallet::new(&server).await;
    sdk::unlock(wallet.state.clone(), wallet.request())
        .await
        .unwrap();
    let common = wallet
        .state
        .unlocked_app_state
        .lock()
        .await
        .as_ref()
        .unwrap()
        .common
        .clone();
    let node_store = store_id();
    let fence = server.rows(&node_store)[FENCE].clone();
    let entered = Arc::new(Semaphore::new(0));
    let signal = entered.clone();
    let (resume, wait) = std::sync::mpsc::channel();
    let wait = Mutex::new(wait);
    common.kv_store.set_before_stop_gate_hook(Arc::new(move || {
        signal.add_permits(1);
        wait.lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(10))
            .expect("release stop gate");
    }));
    let first = tokio::spawn(crate::ldk::stop_node(wallet.state.clone()));
    tokio::time::timeout(Duration::from_secs(10), entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    assert!(wallet.state.unlocked_app_state.lock().await.is_none());
    assert!(!first.is_finished());
    tokio::time::timeout(
        Duration::from_secs(1),
        crate::ldk::stop_node(wallet.state.clone()),
    )
    .await
    .expect("second stop owns no session");
    assert_eq!(server.rows(&node_store)[FENCE], fence);
    resume.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), first)
        .await
        .unwrap()
        .unwrap();
    assert!(!server.rows(&node_store).contains_key(FENCE));
    assert!(common.persistence_worker.lock().unwrap().is_none());
}

/// Lock cannot hand over the fence until an already admitted backup commits, even if its caller exits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mainnet_vss_lock_waits_for_canceled_manual_backup_callers() {
    let server = Server::new().await;
    let wallet = Wallet::new(&server).await;
    let node_store = store_id();
    let rgb_store = format!("{node_store}_rgb");
    for use_rest in [false, true] {
        sdk::unlock(wallet.state.clone(), wallet.request())
            .await
            .unwrap();
        let previous = sdk::vss_backup(wallet.state.clone()).await.unwrap();
        let gate = Arc::new(BackupGate {
            store: rgb_store.clone(),
            entered: Semaphore::new(0),
            resume: Semaphore::new(0),
        });
        server.state.lock().unwrap().backup_gate = Some(gate.clone());
        let state = wallet.state.clone();
        let backup = tokio::spawn(async move {
            if use_rest {
                routes::vss_backup(State(state)).await.map(|_| ())
            } else {
                sdk::vss_backup(state).await.map(|_| ())
            }
        });
        tokio::time::timeout(Duration::from_secs(10), gate.entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        assert!(
            wallet.state.unlocked_app_state.try_lock().is_err(),
            "backup must retain the session guard"
        );
        let state = wallet.state.clone();
        let lock = tokio::spawn(async move { routes::lock(State(state)).await });
        backup.abort();
        assert!(backup.await.unwrap_err().is_cancelled());
        assert!(!lock.is_finished());
        assert!(server.rows(&node_store).contains_key(FENCE));
        gate.resume.add_permits(1);
        let _ = tokio::time::timeout(Duration::from_secs(15), lock)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(server.rows(&rgb_store)["backup/data"].version > previous);
        assert!(!server.rows(&node_store).contains_key(FENCE));
        assert!(wallet.state.unlocked_app_state.lock().await.is_none());
        assert!(!*wallet.state.changing_state.lock().unwrap());
        let state = server.state.lock().unwrap();
        let backup_position = state
            .requests
            .iter()
            .rposition(|r| {
                r.store == rgb_store
                    && r.method == "put"
                    && r.keys.iter().any(|key| key == "backup/data")
            })
            .unwrap();
        let release_position = state
            .requests
            .iter()
            .rposition(|r| r.store == node_store && r.deletes.iter().any(|key| key == FENCE))
            .unwrap();
        assert!(
            backup_position < release_position,
            "RGB backup must commit before fence release"
        );
    }
}
