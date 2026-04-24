use super::*;

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn reset_swap_runtime_state_for_tests() {
    WASM_SWAP_RUNTIME_STATE.with(|state| {
        *state.borrow_mut() = SwapRuntimeState::default();
    });
}

#[cfg(target_arch = "wasm32")]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn test_insert_swap_with_payment_hash(payment_hash: &str, taker: bool) {
    let now = now_secs();
    WASM_SWAP_RUNTIME_STATE.with(|state| {
        let mut state = state.borrow_mut();
        ensure_swap_runtime_loaded(&mut state);
        let swap = SwapData {
            qty_from: 1000,
            qty_to: 10,
            from_asset: None,
            to_asset: Some(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            ),
            payment_hash: payment_hash.to_string(),
            status: SwapStatus::Waiting,
            requested_at: now,
            initiated_at: None,
            expires_at: now + 600,
            completed_at: None,
        };
        if taker {
            state.taker.insert(payment_hash.to_string(), swap);
        } else {
            state.maker.insert(payment_hash.to_string(), swap);
        }
        persist_swap_runtime_state(&state);
    });
}

#[cfg(not(target_arch = "wasm32"))]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn test_insert_swap_with_payment_hash(_payment_hash: &str, _taker: bool) {}
