//! Real protobuf client traffic against a versioned local fault-injection server.
//! This proves upload ordering and preservation, not production server authentication/durability.
use super::*;
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use rand::{RngCore, SeedableRng};
use std::{collections::BTreeMap, sync::Mutex};
use vss_client::{
    prost::Message,
    types::{ErrorCode, ErrorResponse, GetObjectResponse, PutObjectResponse},
};

#[derive(Default)]
struct ServerState {
    rows: BTreeMap<String, KeyValue>,
    fail_second_chunk: bool,
    fail_manifest: bool,
    lose_commit_response: bool,
    corrupt_next_readback: bool,
    corrupt_readback: bool,
    advance_manifest_during_delete: bool,
}
fn protobuf(status: StatusCode, value: impl Message) -> Response {
    (
        status,
        [("content-type", "application/octet-stream")],
        value.encode_to_vec(),
    )
        .into_response()
}
fn failure(code: ErrorCode) -> Response {
    let status = match code {
        ErrorCode::NoSuchKeyException => StatusCode::NOT_FOUND,
        ErrorCode::ConflictException => StatusCode::CONFLICT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    protobuf(
        status,
        ErrorResponse {
            error_code: code as i32,
            message: "injected test failure".into(),
        },
    )
}
async fn handle(
    State(state): State<Arc<Mutex<ServerState>>>,
    uri: Uri,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    assert!(headers.contains_key("authorization"));
    let mut state = state.lock().unwrap();
    match uri.path() {
        "/getObject" => {
            let request = GetObjectRequest::decode(bytes).unwrap();
            if state.advance_manifest_during_delete
                && request.key.starts_with(BACKUP_KEY_CHUNK_PREFIX)
            {
                state.advance_manifest_during_delete = false;
                state.rows.get_mut(BACKUP_KEY_MANIFEST).unwrap().version += 1;
            }
            match state.rows.get(&request.key).cloned() {
                Some(mut value) => {
                    if request.key == BACKUP_KEY_MANIFEST && state.corrupt_readback {
                        value.version += 1;
                    }
                    protobuf(StatusCode::OK, GetObjectResponse { value: Some(value) })
                }
                None => failure(ErrorCode::NoSuchKeyException),
            }
        }
        "/putObjects" => {
            let request = PutObjectRequest::decode(bytes).unwrap();
            let manifest_commit = request
                .transaction_items
                .iter()
                .any(|row| row.key == BACKUP_KEY_MANIFEST);
            if state.fail_second_chunk
                && request.transaction_items.iter().any(|row| {
                    row.key.starts_with(BACKUP_KEY_CHUNK_PREFIX) && row.key.ends_with("/1")
                })
            {
                return failure(ErrorCode::InternalServerException);
            }
            if manifest_commit && state.fail_manifest {
                return failure(ErrorCode::ConflictException);
            }
            // Validate every CAS before changing any row, matching VSS all-or-nothing semantics.
            if request
                .transaction_items
                .iter()
                .chain(request.delete_items.iter())
                .any(|row| state.rows.get(&row.key).map_or(0, |old| old.version) != row.version)
            {
                return failure(ErrorCode::ConflictException);
            }
            for mut row in request.transaction_items {
                row.version += 1;
                state.rows.insert(row.key.clone(), row);
            }
            for row in request.delete_items {
                state.rows.remove(&row.key);
            }
            if manifest_commit && state.corrupt_next_readback {
                state.corrupt_next_readback = false;
                state.corrupt_readback = true;
            }
            if manifest_commit && state.lose_commit_response {
                return failure(ErrorCode::InternalServerException);
            }
            protobuf(StatusCode::OK, PutObjectResponse {})
        }
        _ => panic!("unexpected test RPC"),
    }
}
struct Server {
    url: String,
    state: Arc<Mutex<ServerState>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(ServerState::default()));
        let router = Router::new()
            .fallback(post(handle))
            .with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { url, state, task }
    }
    fn client(&self) -> VssBackupClient {
        VssBackupClient::new(VssBackupConfig::new(
            self.url.clone(),
            "test".into(),
            SecretKey::from_slice(&[1; 32]).unwrap(),
        ))
        .unwrap()
    }
    fn manifest(&self) -> KeyValue {
        self.state.lock().unwrap().rows[BACKUP_KEY_MANIFEST].clone()
    }
}
fn payload(seed: u64) -> Vec<u8> {
    let mut value = vec![0u8; VSS_CHUNK_SIZE + 1000];
    rand::rngs::StdRng::seed_from_u64(seed).fill_bytes(&mut value);
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file("aabbccdd/data", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(&value).unwrap();
    zip.finish().unwrap().into_inner()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_chunk_and_manifest_cas_failures_preserve_previous_backup() {
    let server = Server::new().await;
    let client = server.client();
    let first = payload(1);
    assert_eq!(client.upload_backup(first.clone()).await.unwrap(), 1);
    let committed = server.manifest();
    let first_manifest: BackupManifest = serde_json::from_slice(&committed.value).unwrap();
    assert_eq!(client.download_backup().await.unwrap(), first);
    server.state.lock().unwrap().fail_second_chunk = true;
    assert!(client.upload_backup(payload(2)).await.is_err());
    assert_eq!(server.manifest(), committed);
    assert_eq!(client.download_backup().await.unwrap(), first);
    {
        let mut state = server.state.lock().unwrap();
        assert!(state
            .rows
            .keys()
            .any(|key| key.starts_with(BACKUP_KEY_CHUNK_PREFIX)
                && !key.starts_with(&first_manifest.chunk_prefix)));
        state.fail_second_chunk = false;
        state.fail_manifest = true;
    }
    assert!(client.upload_backup(payload(3)).await.is_err());
    assert_eq!(server.manifest(), committed);
    assert_eq!(client.download_backup().await.unwrap(), first);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uncertain_commit_keeps_new_and_old_generations_without_claiming_success() {
    let server = Server::new().await;
    let client = server.client();
    client.upload_backup(payload(1)).await.unwrap();
    let old: BackupManifest = serde_json::from_slice(&server.manifest().value).unwrap();
    server.state.lock().unwrap().lose_commit_response = true;
    let next = payload(2);
    assert!(client.upload_backup(next.clone()).await.is_err());
    assert_eq!(client.download_backup().await.unwrap(), next);
    assert!(server
        .state
        .lock()
        .unwrap()
        .rows
        .keys()
        .any(|key| key.starts_with(&old.chunk_prefix)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mismatched_canonical_readback_cannot_acknowledge_local_wallet() {
    use rgb_lib::{
        keys::{generate_keys, WitnessVersion},
        wallet::{DatabaseType, RgbWalletOpsOffline, SinglesigKeys, Wallet, WalletData},
        AssetSchema, BitcoinNetwork,
    };
    let server = Server::new().await;
    let client = server.client();
    let directory = tempfile::tempdir().unwrap();
    let keys = generate_keys(BitcoinNetwork::Regtest, WitnessVersion::Taproot);
    let mut wallet = Wallet::new(
        WalletData {
            data_dir: directory.path().to_str().unwrap().into(),
            bitcoin_network: BitcoinNetwork::Regtest,
            database_type: DatabaseType::Sqlite,
            max_allocations_per_utxo: 1,
            supported_schemas: vec![AssetSchema::Nia],
        },
        SinglesigKeys::from_keys(&keys, None),
    )
    .unwrap();
    backup_wallet(&mut wallet, &client).unwrap();
    assert!(!wallet.backup_info().unwrap());
    wallet.get_address().unwrap();
    assert!(wallet.backup_info().unwrap());
    server.state.lock().unwrap().corrupt_next_readback = true;
    assert!(backup_wallet(&mut wallet, &client).is_err());
    assert!(wallet.backup_info().unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_current_generation_cannot_delete_a_newer_manifest() {
    let server = Server::new().await;
    let client = server.client();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file("aabbccdd/data", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"small synthetic wallet snapshot").unwrap();
    let snapshot = zip.finish().unwrap().into_inner();
    client.upload_backup(snapshot.clone()).await.unwrap();
    let manifest: BackupManifest = serde_json::from_slice(&server.manifest().value).unwrap();
    assert_eq!(manifest.chunk_count, 1);
    server.state.lock().unwrap().advance_manifest_during_delete = true;
    assert!(matches!(
        client.delete_backup().await,
        Err(Error::VssVersionConflict { .. })
    ));
    assert_eq!(client.download_backup().await.unwrap(), snapshot);
    client.delete_backup().await.unwrap();
    assert!(server.state.lock().unwrap().rows.is_empty());
    client.delete_backup().await.unwrap();
}
