use std::future::Future;
use std::net::SocketAddr;
use std::net::ToSocketAddrs;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bitcoin::secp256k1::PublicKey;
use rgb_lib::bdk_wallet::keys::bip39::Mnemonic;

use crate::disk;
use crate::error::APIError;
use crate::ldk::{self, LdkBackgroundServices, PeerManager};
use crate::utils::{AppState, UnlockedAppState};

pub(crate) trait PeerConnectivity: Send + Sync {
    fn connect_peer_if_necessary<'a>(
        &'a self,
        pubkey: PublicKey,
        address: SocketAddr,
        peer_manager: Arc<PeerManager>,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send + 'a>>;

    fn has_peer(&self, peer_pubkey: &PublicKey, peer_manager: &Arc<PeerManager>) -> bool;

    fn disconnect_peer(&self, peer_pubkey: PublicKey, peer_manager: Arc<PeerManager>);

    fn resolve_connected_peer_addr(
        &self,
        peer_pubkey: &PublicKey,
        peer_manager: &Arc<PeerManager>,
    ) -> Option<SocketAddr>;
}

pub(crate) trait ChannelPeerStore: Send + Sync {
    fn persist_channel_peer(
        &self,
        store_path: &Path,
        peer_pubkey: &PublicKey,
        peer_addr: &SocketAddr,
    ) -> Result<(), APIError>;

    fn delete_channel_peer(
        &self,
        store_path: &Path,
        peer_pubkey_hex: String,
    ) -> Result<(), APIError>;

    fn read_channel_peer_data(
        &self,
        store_path: &Path,
    ) -> Result<std::collections::HashMap<PublicKey, SocketAddr>, APIError>;
}

pub(crate) trait LdkLifecycle: Send + Sync {
    fn start_ldk<'a>(
        &'a self,
        state: Arc<AppState>,
        mnemonic: Mnemonic,
        request: crate::core_types::UnlockRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<(LdkBackgroundServices, Arc<UnlockedAppState>), APIError>>
                + Send
                + 'a,
        >,
    >;

    fn stop_ldk<'a>(
        &'a self,
        state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

pub(crate) trait Scheduler: Send + Sync {
    fn sleep<'a>(&'a self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NativePeerConnectivity;

impl PeerConnectivity for NativePeerConnectivity {
    fn connect_peer_if_necessary<'a>(
        &'a self,
        pubkey: PublicKey,
        address: SocketAddr,
        peer_manager: Arc<PeerManager>,
    ) -> Pin<Box<dyn Future<Output = Result<(), APIError>> + Send + 'a>> {
        Box::pin(async move {
            let scheduler = native_scheduler();
            for peer_details in peer_manager.list_peers() {
                if peer_details.counterparty_node_id == pubkey {
                    return Ok(());
                }
            }
            native_do_connect_peer(pubkey, address, peer_manager, &scheduler).await?;
            tracing::info!("connected to peer (pubkey: {pubkey}, addr: {address})");
            Ok(())
        })
    }

    fn has_peer(&self, peer_pubkey: &PublicKey, peer_manager: &Arc<PeerManager>) -> bool {
        peer_manager.peer_by_node_id(peer_pubkey).is_some()
    }

    fn disconnect_peer(&self, peer_pubkey: PublicKey, peer_manager: Arc<PeerManager>) {
        peer_manager.disconnect_by_node_id(peer_pubkey);
    }

    fn resolve_connected_peer_addr(
        &self,
        peer_pubkey: &PublicKey,
        peer_manager: &Arc<PeerManager>,
    ) -> Option<SocketAddr> {
        let peer = peer_manager.peer_by_node_id(peer_pubkey)?;
        let socket_address = peer.socket_address?;
        let mut socket_addrs = socket_address.to_socket_addrs().ok()?;
        socket_addrs.next()
    }
}

async fn native_do_connect_peer(
    pubkey: PublicKey,
    address: SocketAddr,
    peer_manager: Arc<PeerManager>,
    scheduler: &dyn Scheduler,
) -> Result<(), APIError> {
    match lightning_net_tokio::connect_outbound(Arc::clone(&peer_manager), pubkey, address).await {
        Some(connection_closed_future) => {
            let mut connection_closed_future = Box::pin(connection_closed_future);
            loop {
                tokio::select! {
                    _ = &mut connection_closed_future => return Err(APIError::FailedPeerConnection),
                    _ = scheduler.sleep(Duration::from_millis(10)) => {},
                };
                if peer_manager.peer_by_node_id(&pubkey).is_some() {
                    return Ok(());
                }
            }
        }
        None => Err(APIError::FailedPeerConnection),
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NativeChannelPeerStore;

impl ChannelPeerStore for NativeChannelPeerStore {
    fn persist_channel_peer(
        &self,
        store_path: &Path,
        peer_pubkey: &PublicKey,
        peer_addr: &SocketAddr,
    ) -> Result<(), APIError> {
        disk::persist_channel_peer(store_path, peer_pubkey, peer_addr)
    }

    fn delete_channel_peer(
        &self,
        store_path: &Path,
        peer_pubkey_hex: String,
    ) -> Result<(), APIError> {
        disk::delete_channel_peer(store_path, peer_pubkey_hex)
    }

    fn read_channel_peer_data(
        &self,
        store_path: &Path,
    ) -> Result<std::collections::HashMap<PublicKey, SocketAddr>, APIError> {
        disk::read_channel_peer_data(store_path)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NativeLdkLifecycle;

impl LdkLifecycle for NativeLdkLifecycle {
    fn start_ldk<'a>(
        &'a self,
        state: Arc<AppState>,
        mnemonic: Mnemonic,
        request: crate::core_types::UnlockRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<(LdkBackgroundServices, Arc<UnlockedAppState>), APIError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move { ldk::start_ldk(state, mnemonic, request).await })
    }

    fn stop_ldk<'a>(
        &'a self,
        state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            ldk::stop_ldk(state).await;
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NativeScheduler;

impl Scheduler for NativeScheduler {
    fn sleep<'a>(&'a self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            tokio::time::sleep(duration).await;
        })
    }
}

pub(crate) fn native_peer_connectivity() -> NativePeerConnectivity {
    NativePeerConnectivity
}

pub(crate) fn native_channel_peer_store() -> NativeChannelPeerStore {
    NativeChannelPeerStore
}

pub(crate) fn native_ldk_lifecycle() -> NativeLdkLifecycle {
    NativeLdkLifecycle
}

pub(crate) fn native_scheduler() -> NativeScheduler {
    NativeScheduler
}
