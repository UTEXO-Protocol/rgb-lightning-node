use wasm_bindgen::prelude::JsValue;

pub(crate) fn event_stream_ownership_enabled(use_runtime_state_for_ln_views: bool) -> bool {
    use_runtime_state_for_ln_views
}

pub(crate) fn ensure_manual_status_update_allowed(
    use_runtime_state_for_ln_views: bool,
) -> Result<(), JsValue> {
    if event_stream_ownership_enabled(use_runtime_state_for_ln_views) && !manual_event_debug_mode()
    {
        return Err(JsValue::from_str(
            "manual payment status updates are disabled on wasm_native_ldk runtime; rely on runtime event stream",
        ));
    }
    Ok(())
}

pub(crate) fn ensure_manual_event_ingestion_allowed(
    use_runtime_state_for_ln_views: bool,
) -> Result<(), JsValue> {
    if event_stream_ownership_enabled(use_runtime_state_for_ln_views) && !manual_event_debug_mode()
    {
        return Err(JsValue::from_str(
            "manual runtime event ingestion is disabled on wasm_native_ldk runtime; rely on runtime event stream",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/ldk_event_applier_tests.rs"]
mod tests;

#[cfg(any(test, feature = "debug-manual-events"))]
fn manual_event_debug_mode() -> bool {
    true
}

#[cfg(not(any(test, feature = "debug-manual-events")))]
fn manual_event_debug_mode() -> bool {
    false
}
