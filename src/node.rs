use std::sync::Arc;

use rgb_lib::BitcoinNetwork;

use crate::args::UserArgs;
use crate::error::AppError;
use crate::ldk::stop_ldk;
use crate::utils::{start_daemon, AppState};

pub struct NodeConfig {
    pub storage_dir_path: std::path::PathBuf,
    pub daemon_listening_port: u16,
    pub ldk_peer_listening_port: u16,
    pub network: BitcoinNetwork,
    pub max_media_upload_size_mb: u16,
    /// Max aggregate size of RGB media accepted over p2p per channel-open (in MB).
    pub max_aggregated_media_size_per_channel_mb: u16,
    /// Max number of pending channel-open consignments buffered over p2p at once. This is a
    /// node-wide cap counted across all peers.
    pub max_pending_consignments: usize,
    /// Max number of RGB media files accepted over p2p per channel-open.
    pub max_media_files_per_channel: usize,
    pub root_public_key: Option<biscuit_auth::PublicKey>,
    pub enable_virtual_channels_v0: bool,
    pub virtual_peer_pubkeys: Vec<bitcoin::secp256k1::PublicKey>,
    pub lsp_base_url: Option<String>,
    pub lsp_bearer_token: Option<String>,
    pub vss_url: Option<String>,
    pub vss_allow_empty_restore: bool,
    pub reuse_addresses: bool,
    /// Socket address of the remote external signer daemon to connect to (Option A). Required to
    /// unlock in external-signer mode — without it, `NodeHandle`-embedding callers can configure
    /// external-signer mode (via `key_source.json`) but can never actually unlock, since there is no
    /// other way to reach the daemon. Always present (`None` when `remote-signer` isn't compiled in).
    pub remote_signer_listen_addr: Option<std::net::SocketAddr>,
}

#[derive(Clone)]
pub struct NodeHandle {
    state: Arc<AppState>,
}

impl NodeHandle {
    #[cfg(feature = "uniffi")]
    pub(crate) fn from_app_state(state: Arc<AppState>) -> Self {
        Self { state }
    }

    #[cfg(feature = "uniffi")]
    pub(crate) fn app_state(&self) -> Arc<AppState> {
        self.state.clone()
    }

    pub async fn new(config: NodeConfig) -> Result<Self, AppError> {
        let args = UserArgs {
            storage_dir_path: config.storage_dir_path,
            daemon_listening_port: config.daemon_listening_port,
            ldk_peer_listening_port: config.ldk_peer_listening_port,
            network: config.network,
            max_media_upload_size_mb: config.max_media_upload_size_mb,
            max_aggregated_media_size_per_channel_mb: config
                .max_aggregated_media_size_per_channel_mb,
            max_pending_consignments: config.max_pending_consignments,
            max_media_files_per_channel: config.max_media_files_per_channel,
            root_public_key: config.root_public_key,
            enable_virtual_channels_v0: config.enable_virtual_channels_v0,
            virtual_peer_pubkeys: config.virtual_peer_pubkeys,
            lsp_base_url: config.lsp_base_url,
            lsp_bearer_token: config.lsp_bearer_token,
            vss_url: config.vss_url,
            vss_allow_empty_restore: config.vss_allow_empty_restore,
            reuse_addresses: config.reuse_addresses,
            remote_signer_listen_addr: config.remote_signer_listen_addr,
            config: Default::default(),
        };
        let state = start_daemon(&args).await?;
        Ok(Self { state })
    }

    pub async fn shutdown(&self) {
        self.state.cancel_token.cancel();
        stop_ldk(self.state.clone()).await;
    }

    #[cfg(feature = "uniffi")]
    pub fn register_for_uniffi(&self) {
        crate::set_uniffi_app_state(self.state.clone());
    }

    #[cfg(feature = "uniffi")]
    pub fn unregister_for_uniffi(&self) {
        crate::clear_uniffi_app_state();
    }
}

#[cfg(all(test, feature = "uniffi"))]
mod tests {
    use super::*;

    /// A `NodeConfig` embedder (a direct Rust consumer, not the uniffi FFI surface — see the `None`
    /// left in `uniffi_api::handle_from_request`) must be able to configure the remote-signer daemon
    /// address; without this field, `NodeHandle`-embedding callers could configure external-signer
    /// mode but could never actually unlock, since there'd be no way to reach the daemon. The field is
    /// unconditional (not `#[cfg(feature = "remote-signer")]`), so this threading works — and this
    /// test runs — regardless of whether that feature is compiled in.
    #[tokio::test]
    async fn node_handle_new_threads_remote_signer_listen_addr_through() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let addr: std::net::SocketAddr = "127.0.0.1:9737".parse().expect("addr");

        let handle = NodeHandle::new(NodeConfig {
            storage_dir_path: tmp.path().to_path_buf(),
            daemon_listening_port: 0,
            ldk_peer_listening_port: 0,
            network: BitcoinNetwork::Regtest,
            max_media_upload_size_mb: 1,
            max_aggregated_media_size_per_channel_mb:
                crate::rgb_file_transfer::MAX_MEDIA_MB_PER_CHANNEL,
            max_pending_consignments: crate::rgb_file_transfer::MAX_PENDING_CONSIGNMENTS,
            max_media_files_per_channel: crate::rgb_file_transfer::MAX_MEDIA_FILES_PER_CHANNEL,
            root_public_key: None,
            enable_virtual_channels_v0: false,
            virtual_peer_pubkeys: vec![],
            lsp_base_url: None,
            lsp_bearer_token: None,
            vss_url: None,
            vss_allow_empty_restore: false,
            reuse_addresses: false,
            remote_signer_listen_addr: Some(addr),
        })
        .await
        .expect("node handle new");

        assert_eq!(
            handle.app_state().static_state.remote_signer_listen_addr,
            Some(addr),
            "NodeConfig::remote_signer_listen_addr did not thread through to StaticState"
        );
    }
}
