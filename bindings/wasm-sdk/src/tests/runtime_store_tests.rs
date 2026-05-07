use super::RUNTIME_STATE_HYDRATE_PREFIXES;

#[test]
fn hydrate_prefixes_cover_runtime_state_domains() {
    let must_include = [
        crate::wasm_node_persistence::WASM_LDK_RUNTIME_STORAGE_PREFIX,
        "rln:wasm:swap-runtime:",
        "rln:wasm:media:",
        "rln:wasm:wallet-rgb-proxy:",
        crate::wasm_node_persistence::WASM_LN_RUNTIME_CORE_STORAGE_PREFIX,
        crate::wasm_node_persistence::WASM_CHAIN_SYNC_STORAGE_PREFIX,
        crate::wasm_node_persistence::WASM_LDK_BROADCAST_QUEUE_STORAGE_PREFIX,
        crate::wasm_node_persistence::WASM_LDK_MONITORS_STORAGE_PREFIX,
        crate::wasm_node_persistence::WASM_RUNTIME_EVENTS_STORAGE_PREFIX,
        crate::wasm_node_persistence::WASM_RGB_LN_TRANSFERS_STORAGE_PREFIX,
        crate::wasm_node_persistence::WASM_VIRTUAL_CHANNELS_V0_STORAGE_PREFIX,
        crate::wasm_node_persistence::WASM_PEER_SESSIONS_STORAGE_PREFIX,
    ];
    for prefix in must_include {
        assert!(
            RUNTIME_STATE_HYDRATE_PREFIXES.contains(&prefix),
            "missing hydrate prefix: {prefix}"
        );
    }
}
