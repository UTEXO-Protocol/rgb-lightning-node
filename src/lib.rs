#![cfg_attr(feature = "uniffi", allow(clippy::empty_line_after_doc_comments))]
#![allow(dead_code)]
#![allow(unused_imports)]

#[cfg(not(any(feature = "electrum", feature = "esplora")))]
compile_error!("at least one of the `electrum` and `esplora` features needs to be enabled");

#[cfg(not(any(feature = "block-sync", feature = "transaction-sync")))]
compile_error!(
    "at least one of the `block-sync` and `transaction-sync` features needs to be enabled"
);

// the generated bindings cannot express cargo-feature gating on `LdkChainSync`, so they need
// both sync backends compiled in
#[cfg(all(
    feature = "uniffi",
    not(all(feature = "block-sync", feature = "transaction-sync"))
))]
compile_error!("the `uniffi` bindings require both `block-sync` and `transaction-sync`");

mod apay_merkle;
mod args;
mod asset_link;
#[cfg(feature = "vss")]
mod async_kv_store;
mod async_order;
mod auth;
mod backup;
mod config;
mod core_types;
mod crypto;
mod custom_msg_rpc;
mod database;
mod disk;
mod error;
#[cfg(test)]
#[path = "test/fee_mock.rs"]
mod fee_mock;
#[cfg(feature = "uniffi")]
pub mod ffi;
mod gossip;
mod kv_store;
mod ldk;
mod ldk_chain_backend;
mod node;
mod rgb;
mod rgb_file_transfer;
mod routes;
mod runtime;
mod sdk;
mod signer;
#[cfg(all(feature = "uniffi", feature = "test-utils"))]
pub mod signer_integration_wire;
mod swap;
mod synced_kv_store;
#[cfg(feature = "test-utils")]
pub mod test_utils;
#[cfg(feature = "uniffi")]
mod uniffi_api;
mod utils;
#[cfg(feature = "vss")]
mod vss_kv_store;

pub use node::{NodeConfig, NodeHandle};

/// Restricted-permission file/dir helpers for the `rln-signer-daemon` binary:
/// `write_restricted_file` atomically creates a file 0600 (no window where it exists with broader
/// permissions, unlike `fs::write` followed by a `chmod`); `check_restricted_file` verifies an
/// existing secret file is a regular owner-only file before trusting it.
#[cfg(feature = "remote-signer")]
pub use signer::key_source::{check_restricted_file, write_restricted_file};
/// Remote external signer daemon (Option A) entry points, for the `rln-signer-daemon` binary.
#[cfg(feature = "remote-signer")]
pub use signer::remote::daemon::{
    run as run_signer_daemon, DaemonBootstrap, DaemonConfig, DaemonSigner, DaemonTlsConfig,
};

#[cfg(feature = "uniffi")]
pub use uniffi_api::*;
#[cfg(feature = "uniffi")]
pub(crate) use uniffi_api::{clear_uniffi_app_state, set_uniffi_app_state};
