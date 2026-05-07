//! Curated public API surface for the WASM SDK.
//!
//! This module is the intended import point for downstream crates and helps keep `lib.rs`
//! focused on wasm-bindgen entrypoints and internal wiring.

// Primary JS-facing SDK entrypoints (wasm-bindgen).
pub use crate::{
    RlnRgbKeysData, RlnWasmInitData, RlnWasmInvoice, RlnWasmRgbProxyTransportConfigData,
    RlnWasmSdk, RlnWasmSdkNodeHandle, RlnWasmSdkRuntimeCapabilitiesData, RlnWasmSdkWalletHandle,
    RlnWasmWallet, WasmRlnNetwork,
};

// Node and runtime types used by demos/E2E and advanced callers.
pub use crate::chain_sync::{
    RlnWasmChainRebroadcastTxData, RlnWasmChainSyncStatusData, WasmChainSyncDriver,
};
pub use crate::ldk_runtime::{
    LdkRuntimeChannelStateData, LdkRuntimeComponentsStatusData, LdkRuntimeFundingRequestData,
    LdkRuntimeFundingTxSubmissionData, LdkRuntimeOpenChannelRequestData,
    LdkRuntimeOpenChannelResultData, LdkRuntimePeerStateData, LdkRuntimeStatusData,
    LdkRuntimeVirtualChannelSessionData, LdkRuntimeVirtualChannelSessionStatusData,
};
pub use crate::ln_node::RlnWasmNode;
