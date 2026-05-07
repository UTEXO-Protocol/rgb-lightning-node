//! Mutiny-style persistence: one stable [`RuntimeScopeKeys`] fans out to every
//! LN-adjacent localStorage key (LDK snapshot — including channel manager / monitor
//! envelope inside the checkpoint — native runtime core, chain sync, runtime events,
//! peer sessions, RGB transfers, virtual-channel flag) so reload cannot drift across modules.

use serde::{de::DeserializeOwned, Serialize};
use wasm_bindgen::JsValue;

use crate::runtime_store::RuntimeStateStore;

pub(crate) const WASM_LDK_RUNTIME_STORAGE_PREFIX: &str = "rln:wasm:ldk-runtime:";
pub(crate) const WASM_LN_RUNTIME_CORE_STORAGE_PREFIX: &str = "rln:wasm:ln-runtime-core:";
pub(crate) const WASM_CHAIN_SYNC_STORAGE_PREFIX: &str = "rln:wasm:chain-sync:";
pub(crate) const WASM_LDK_BROADCAST_QUEUE_STORAGE_PREFIX: &str = "rln:wasm:ldk-broadcast-queue:";
pub(crate) const WASM_LDK_MONITORS_STORAGE_PREFIX: &str = "rln:wasm:ldk-monitors:";
pub(crate) const WASM_RUNTIME_EVENTS_STORAGE_PREFIX: &str = "rln:wasm:runtime-events:";
pub(crate) const WASM_RGB_LN_TRANSFERS_STORAGE_PREFIX: &str = "rln:wasm:rgb-ln-transfers:";
pub(crate) const WASM_PEER_SESSIONS_STORAGE_PREFIX: &str = "rln:wasm:peer-sessions:";
pub(crate) const WASM_VIRTUAL_CHANNELS_V0_STORAGE_PREFIX: &str = "rln:wasm:virtual-channels-v0:";

/// Precomputed storage strings for one logical node instance.
#[derive(Clone, Debug)]
pub(crate) struct RuntimeScopeKeys {
    pub runtime_scope_key: String,
    /// Passed to [`crate::ldk_runtime::ldk_runtime_manager`] and native runtime / chain-sync.
    pub ldk_manager_registry_key: String,
    pub runtime_events_storage_key: String,
    pub rgb_ln_transfers_storage_key: String,
    pub peer_sessions_storage_key: String,
    pub virtual_channels_v0_storage_key: String,
    /// Same base key [`NativeLnRuntimeCore`](crate::NativeLnRuntimeCore) derives internally.
    #[allow(dead_code)]
    pub native_ln_runtime_core_storage_base: String,
    /// Same key [`WasmChainSyncDriver`](crate::chain_sync::WasmChainSyncDriver) derives internally.
    #[allow(dead_code)]
    pub chain_sync_storage_key: String,
    /// Committed LDK snapshot localStorage key (matches [`BrowserBackedLdkRuntimeStorage`](crate::ldk_runtime) layout).
    #[allow(dead_code)]
    pub ldk_runtime_committed_storage_key: String,
    #[allow(dead_code)]
    pub ldk_runtime_pending_storage_key: String,
}

impl RuntimeScopeKeys {
    pub fn from_runtime_scope_key(runtime_scope_key: String) -> Self {
        let ldk_manager_registry_key = format!("node-runtime:{runtime_scope_key}");
        let native_ln_runtime_core_storage_base =
            format!("{WASM_LN_RUNTIME_CORE_STORAGE_PREFIX}{ldk_manager_registry_key}");
        let chain_sync_storage_key =
            format!("{WASM_CHAIN_SYNC_STORAGE_PREFIX}{ldk_manager_registry_key}");
        let ldk_runtime_committed_storage_key =
            format!("{WASM_LDK_RUNTIME_STORAGE_PREFIX}{ldk_manager_registry_key}");
        let ldk_runtime_pending_storage_key =
            format!("{WASM_LDK_RUNTIME_STORAGE_PREFIX}{ldk_manager_registry_key}:pending");
        Self {
            runtime_events_storage_key: format!(
                "{WASM_RUNTIME_EVENTS_STORAGE_PREFIX}{runtime_scope_key}"
            ),
            rgb_ln_transfers_storage_key: format!(
                "{WASM_RGB_LN_TRANSFERS_STORAGE_PREFIX}{runtime_scope_key}"
            ),
            peer_sessions_storage_key: format!(
                "{WASM_PEER_SESSIONS_STORAGE_PREFIX}{runtime_scope_key}"
            ),
            virtual_channels_v0_storage_key: format!(
                "{WASM_VIRTUAL_CHANNELS_V0_STORAGE_PREFIX}{runtime_scope_key}"
            ),
            runtime_scope_key,
            ldk_manager_registry_key,
            native_ln_runtime_core_storage_base,
            chain_sync_storage_key,
            ldk_runtime_committed_storage_key,
            ldk_runtime_pending_storage_key,
        }
    }
}

/// JSON helpers on top of [`RuntimeStateStore`] for WASM node persistence.
pub(crate) trait JsonRuntimeStateStore: RuntimeStateStore {
    fn get_json<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, JsValue> {
        match self.get(key)? {
            Some(raw) => serde_json::from_str(&raw)
                .map(Some)
                .map_err(|e| JsValue::from_str(&format!("json decode: {e}"))),
            None => Ok(None),
        }
    }

    fn set_json<T: Serialize>(&self, key: &str, value: &T) -> Result<(), JsValue> {
        let raw = serde_json::to_string(value)
            .map_err(|e| JsValue::from_str(&format!("json encode: {e}")))?;
        self.set(key, &raw)
    }
}

impl<T: RuntimeStateStore + ?Sized> JsonRuntimeStateStore for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_keys_match_legacy_formulas() {
        let scope = "ws://127.0.0.1:3001#runtime:abc".to_string();
        let keys = RuntimeScopeKeys::from_runtime_scope_key(scope.clone());
        assert_eq!(keys.runtime_scope_key, scope);
        assert_eq!(
            keys.ldk_manager_registry_key,
            format!("node-runtime:{scope}")
        );
        assert_eq!(
            keys.runtime_events_storage_key,
            format!("{WASM_RUNTIME_EVENTS_STORAGE_PREFIX}{scope}")
        );
        assert_eq!(
            keys.rgb_ln_transfers_storage_key,
            format!("{WASM_RGB_LN_TRANSFERS_STORAGE_PREFIX}{scope}")
        );
        assert_eq!(
            keys.peer_sessions_storage_key,
            format!("{WASM_PEER_SESSIONS_STORAGE_PREFIX}{scope}")
        );
        assert_eq!(
            keys.virtual_channels_v0_storage_key,
            format!("{WASM_VIRTUAL_CHANNELS_V0_STORAGE_PREFIX}{scope}")
        );
        assert_eq!(
            keys.native_ln_runtime_core_storage_base,
            format!("{WASM_LN_RUNTIME_CORE_STORAGE_PREFIX}node-runtime:{scope}")
        );
        assert_eq!(
            keys.chain_sync_storage_key,
            format!("{WASM_CHAIN_SYNC_STORAGE_PREFIX}node-runtime:{scope}")
        );
        assert_eq!(
            keys.ldk_runtime_committed_storage_key,
            format!("{WASM_LDK_RUNTIME_STORAGE_PREFIX}node-runtime:{scope}")
        );
        assert_eq!(
            keys.ldk_runtime_pending_storage_key,
            format!("{WASM_LDK_RUNTIME_STORAGE_PREFIX}node-runtime:{scope}:pending")
        );
        assert_eq!(
            keys.native_ln_runtime_core_storage_base,
            format!("{WASM_LN_RUNTIME_CORE_STORAGE_PREFIX}node-runtime:{scope}")
        );
        assert_eq!(
            keys.chain_sync_storage_key,
            format!("{WASM_CHAIN_SYNC_STORAGE_PREFIX}node-runtime:{scope}")
        );
    }
}
