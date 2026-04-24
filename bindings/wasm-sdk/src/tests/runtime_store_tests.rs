use super::RUNTIME_STATE_HYDRATE_PREFIXES;

#[test]
fn hydrate_prefixes_cover_runtime_state_domains() {
    let must_include = [
        "rln:wasm:ldk-runtime:",
        "rln:wasm:swap-runtime:",
        "rln:wasm:media:",
        "rln:wasm:wallet-rgb-proxy:",
        "rln:wasm:ln-runtime-core:",
        "rln:wasm:chain-sync:",
        "rln:wasm:runtime-events:",
        "rln:wasm:rgb-ln-transfers:",
    ];
    for prefix in must_include {
        assert!(
            RUNTIME_STATE_HYDRATE_PREFIXES.contains(&prefix),
            "missing hydrate prefix: {prefix}"
        );
    }
}
