use std::collections::HashSet;
use std::sync::{Arc, Mutex, RwLock};

use sea_orm::{ConnectOptions, Database};
use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;

use crate::disk::FilesystemLogger;
use crate::error::APIError;
use crate::utils::{AppState, StaticState};
use crate::{NodeHandle, RlnError};

#[cfg(feature = "uniffi")]
use bitcoin::hex::DisplayHex;

pub struct TestAppState(Arc<AppState>);

pub fn mock_locked_app_state() -> TestAppState {
    let tmp = tempfile::tempdir().expect("tempdir for mock state");
    let path = tmp.keep();

    let db_path = path.join("rln_db");
    let connection_string = format!("sqlite:{}?mode=rwc", db_path.display());
    let database =
        crate::runtime::block_on(Database::connect(ConnectOptions::new(connection_string)))
            .expect("mock database connection");

    TestAppState(Arc::new(AppState {
        static_state: Arc::new(StaticState {
            config: Default::default(),
            ldk_peer_listening_port: 9735,
            network: rgb_lib::BitcoinNetwork::Regtest,
            storage_dir_path: path.clone(),
            ldk_data_dir: path.join(".ldk"),
            logger: Arc::new(FilesystemLogger::new(path)),
            max_media_upload_size_mb: 1,
            max_aggregated_media_size_per_channel_mb:
                crate::rgb_file_transfer::MAX_MEDIA_MB_PER_CHANNEL,
            max_pending_consignments: crate::rgb_file_transfer::MAX_PENDING_CONSIGNMENTS,
            max_media_files_per_channel: crate::rgb_file_transfer::MAX_MEDIA_FILES_PER_CHANNEL,
            enable_virtual_channels_v0: false,
            virtual_peer_pubkeys: vec![],
            database: RwLock::new(Arc::new(database)),
            lsp_base_url: None,
            lsp_bearer_token: None,
            vss_url: None,
            vss_allow_empty_restore: false,
            reuse_addresses: false,
            remote_signer_listen_addr: None,
        }),
        cancel_token: CancellationToken::new(),
        unlocked_app_state: Arc::new(TokioMutex::new(None)),
        ldk_background_services: Arc::new(Mutex::new(None)),
        attached_external_signer: Arc::new(Mutex::new(None)),
        changing_state: Mutex::new(false),
        root_public_key: None,
        revoked_tokens: Arc::new(Mutex::new(HashSet::new())),
    }))
}

pub fn register_uniffi_state_for_tests(state: &TestAppState) {
    crate::set_uniffi_app_state(state.0.clone());
}

pub fn clear_uniffi_state_for_tests() {
    crate::clear_uniffi_app_state();
}

pub fn node_handle_from_mock_state_for_tests(state: &TestAppState) -> NodeHandle {
    NodeHandle::from_app_state(state.0.clone())
}

#[cfg(feature = "uniffi")]
pub fn channel_has_inflight_htlcs(
    node: &crate::SdkNode,
    channel_id: crate::ChannelId,
) -> Result<bool, RlnError> {
    let channel_id = channel_id.0.as_hex().to_string();
    crate::uniffi_api::channel_has_inflight_htlcs_for_tests(node, &channel_id)
}

pub fn processed_channel_ready_event_participants(channel_id: crate::ChannelId) -> usize {
    crate::ldk::processed_channel_ready_event_participants(&channel_id)
}

pub struct ErrorMappingSnapshot {
    pub locked_node: RlnError,
    pub payment_not_found: RlnError,
    pub io_error: RlnError,
}

pub fn error_mapping_snapshot_for_tests() -> ErrorMappingSnapshot {
    ErrorMappingSnapshot {
        locked_node: crate::uniffi_api::state::map_api_error(APIError::LockedNode),
        payment_not_found: crate::uniffi_api::state::map_api_error(APIError::PaymentNotFound(
            "x".to_string(),
        )),
        io_error: crate::uniffi_api::state::map_api_error(APIError::IO(std::io::Error::other(
            "boom",
        ))),
    }
}
