#![cfg_attr(feature = "uniffi", allow(clippy::empty_line_after_doc_comments))]
#![allow(dead_code)]
#![allow(unused_imports)]

#[cfg(not(target_arch = "wasm32"))]
mod args;
#[cfg(not(target_arch = "wasm32"))]
mod auth;
#[cfg(not(target_arch = "wasm32"))]
mod backup;
#[cfg(not(target_arch = "wasm32"))]
mod bitcoind;
#[cfg(not(target_arch = "wasm32"))]
mod core_types;
#[cfg(not(target_arch = "wasm32"))]
mod disk;
#[cfg(not(target_arch = "wasm32"))]
mod error;
#[cfg(test)]
#[path = "test/fee_mock.rs"]
mod fee_mock;
#[cfg(all(feature = "uniffi", not(target_arch = "wasm32")))]
pub mod ffi;
#[cfg(not(target_arch = "wasm32"))]
mod ldk;
#[cfg(not(target_arch = "wasm32"))]
mod node;
#[cfg(not(target_arch = "wasm32"))]
mod rgb;
#[cfg(not(target_arch = "wasm32"))]
mod sdk;
#[cfg(target_arch = "wasm32")]
#[path = "sdk/wasm.rs"]
pub mod sdk;
#[cfg(not(target_arch = "wasm32"))]
mod swap;
#[cfg(feature = "test-utils")]
pub mod test_utils;
#[cfg(all(feature = "uniffi", not(target_arch = "wasm32")))]
mod uniffi_api;
#[cfg(not(target_arch = "wasm32"))]
mod utils;

#[cfg(not(target_arch = "wasm32"))]
pub use node::{NodeConfig, NodeHandle};

#[cfg(target_arch = "wasm32")]
pub struct NodeConfig;

#[cfg(target_arch = "wasm32")]
pub struct NodeHandle;

#[cfg(all(feature = "uniffi", not(target_arch = "wasm32")))]
pub use uniffi_api::*;
#[cfg(all(feature = "uniffi", not(target_arch = "wasm32")))]
pub(crate) use uniffi_api::{clear_uniffi_app_state, set_uniffi_app_state};
