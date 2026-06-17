use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::str::FromStr;

use js_sys::Function;
use secp256k1::PublicKey as SecpPublicKey;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

use crate::ln_transport::{
    commit_last_applied_seq, ln_socket_connect, ln_socket_connect_with_options,
    replay_frame_disposition, ReplayFrameDisposition, RlnWasmLnSocket,
    RlnWasmReplayInboundFrameData,
};

#[cfg(test)]
#[path = "tests/peer_session_tests.rs"]
mod tests;

#[inline]
fn peer_session_debug(msg: &str) {
    #[cfg(target_arch = "wasm32")]
    web_sys::console::log_1(&JsValue::from_str(msg));
    #[cfg(not(target_arch = "wasm32"))]
    let _ = msg;
}

#[cfg(target_arch = "wasm32")]
async fn sleep_ms(ms: u32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(window) = web_sys::window() {
            let _ =
                window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32);
        } else {
            let _ = resolve.call0(&JsValue::NULL);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

#[cfg(not(target_arch = "wasm32"))]
async fn sleep_ms(ms: u32) {
    std::thread::sleep(std::time::Duration::from_millis(ms as u64));
}

trait PeerManagerAdapter {
    fn new_outbound_connection(&self, peer_pubkey: &str) -> Result<String, JsValue>;
    fn read_event(&self, payload_hex: &str) -> Result<(), JsValue>;
    fn process_events(&self) -> Result<(), JsValue>;
    fn socket_disconnected(&self) -> Result<(), JsValue>;
    fn take_outbound_frames(&self) -> Result<Vec<String>, JsValue>;
    fn report_error(&self, error_message: &str) -> Result<(), JsValue>;
}

struct JsPeerManagerAdapter {
    new_outbound_connection_cb: Function,
    read_event_cb: Function,
    process_events_cb: Function,
    socket_disconnected_cb: Function,
    report_error_cb: Function,
}

impl PeerManagerAdapter for JsPeerManagerAdapter {
    fn new_outbound_connection(&self, peer_pubkey: &str) -> Result<String, JsValue> {
        let res = self
            .new_outbound_connection_cb
            .call1(&JsValue::NULL, &JsValue::from_str(peer_pubkey))?;
        res.as_string().ok_or_else(|| {
            JsValue::from_str(sdk_contracts::ERR_NEW_OUTBOUND_CONNECTION_CB_HEX_STRING)
        })
    }

    fn read_event(&self, payload_hex: &str) -> Result<(), JsValue> {
        let _ = self
            .read_event_cb
            .call1(&JsValue::NULL, &JsValue::from_str(payload_hex))?;
        Ok(())
    }

    fn process_events(&self) -> Result<(), JsValue> {
        let _ = self.process_events_cb.call0(&JsValue::NULL)?;
        Ok(())
    }

    fn socket_disconnected(&self) -> Result<(), JsValue> {
        let _ = self.socket_disconnected_cb.call0(&JsValue::NULL)?;
        Ok(())
    }

    fn take_outbound_frames(&self) -> Result<Vec<String>, JsValue> {
        Ok(Vec::new())
    }

    fn report_error(&self, error_message: &str) -> Result<(), JsValue> {
        let _ = self
            .report_error_cb
            .call1(&JsValue::NULL, &JsValue::from_str(error_message))?;
        Ok(())
    }
}

pub struct RustPeerManagerCallbacks {
    pub new_outbound_connection: Box<dyn Fn(&str) -> Result<String, JsValue>>,
    pub read_event: Box<dyn Fn(&str) -> Result<(), JsValue>>,
    pub process_events: Box<dyn Fn() -> Result<(), JsValue>>,
    pub socket_disconnected: Box<dyn Fn() -> Result<(), JsValue>>,
    pub take_outbound_frames: Box<dyn Fn() -> Result<Vec<String>, JsValue>>,
    pub report_error: Box<dyn Fn(&str) -> Result<(), JsValue>>,
}

pub struct RlnLdkPeerManagerHooks {
    pub new_outbound_connection: Rc<dyn Fn(&str) -> Result<String, JsValue>>,
    pub read_event: Rc<dyn Fn(&str, &str) -> Result<(), JsValue>>,
    pub process_events: Rc<dyn Fn() -> Result<(), JsValue>>,
    pub socket_disconnected: Rc<dyn Fn(&str) -> Result<(), JsValue>>,
    pub take_outbound_frames: Rc<dyn Fn(&str) -> Result<Vec<String>, JsValue>>,
    pub report_error: Rc<dyn Fn(&str) -> Result<(), JsValue>>,
}

thread_local! {
    static RLN_LDK_PEER_MANAGER_HOOKS: RefCell<Option<Rc<RlnLdkPeerManagerHooks>>> = RefCell::new(None);
    static RLN_LDK_PEER_MANAGER_HOOKS_V2_READY: Cell<bool> = const { Cell::new(false) };
}

pub fn install_rln_ldk_peer_manager_hooks(hooks: RlnLdkPeerManagerHooks) {
    RLN_LDK_PEER_MANAGER_HOOKS.with(|slot| {
        slot.replace(Some(Rc::new(hooks)));
    });
    RLN_LDK_PEER_MANAGER_HOOKS_V2_READY.with(|ready| ready.set(true));
}

pub fn clear_rln_ldk_peer_manager_hooks() {
    RLN_LDK_PEER_MANAGER_HOOKS.with(|slot| {
        slot.replace(None);
    });
    RLN_LDK_PEER_MANAGER_HOOKS_V2_READY.with(|ready| ready.set(false));
}

fn get_rln_ldk_peer_manager_hooks() -> Option<Rc<RlnLdkPeerManagerHooks>> {
    RLN_LDK_PEER_MANAGER_HOOKS.with(|slot| slot.borrow().as_ref().cloned())
}

#[wasm_bindgen(js_name = installPeerManagerHooksFromJs)]
pub fn install_peer_manager_hooks_from_js(
    new_outbound_connection_cb: Function,
    read_event_cb: Function,
    process_events_cb: Function,
    socket_disconnected_cb: Function,
    report_error_cb: Function,
) {
    RLN_LDK_PEER_MANAGER_HOOKS_V2_READY.with(|ready| ready.set(false));
    install_rln_ldk_peer_manager_hooks(RlnLdkPeerManagerHooks {
        new_outbound_connection: Rc::new(move |peer_pubkey| {
            let res = new_outbound_connection_cb
                .call1(&JsValue::NULL, &JsValue::from_str(peer_pubkey))?;
            res.as_string().ok_or_else(|| {
                JsValue::from_str(sdk_contracts::ERR_NEW_OUTBOUND_CONNECTION_CB_HEX_STRING)
            })
        }),
        read_event: Rc::new(move |_peer_pubkey, payload_hex| {
            let _ = read_event_cb.call1(&JsValue::NULL, &JsValue::from_str(payload_hex))?;
            Ok(())
        }),
        process_events: Rc::new(move || {
            let _ = process_events_cb.call0(&JsValue::NULL)?;
            Ok(())
        }),
        socket_disconnected: Rc::new(move |_peer_pubkey| {
            let _ = socket_disconnected_cb.call0(&JsValue::NULL)?;
            Ok(())
        }),
        // Legacy installer has no outbound-drain callback parameter.
        // Keep this as a hard error so production flows cannot silently run
        // without real outbound frame progression.
        take_outbound_frames: Rc::new(move |_peer_pubkey| {
            Err(JsValue::from_str(
                "installPeerManagerHooksFromJs is incomplete: missing take_outbound_frames callback; use installPeerManagerHooksFromJsV2",
            ))
        }),
        report_error: Rc::new(move |error_message| {
            let _ = report_error_cb.call1(&JsValue::NULL, &JsValue::from_str(error_message))?;
            Ok(())
        }),
    });
}

#[wasm_bindgen(js_name = installPeerManagerHooksFromJsV2)]
pub fn install_peer_manager_hooks_from_js_v2(
    new_outbound_connection_cb: Function,
    read_event_cb: Function,
    process_events_cb: Function,
    take_outbound_frames_cb: Function,
    socket_disconnected_cb: Function,
    report_error_cb: Function,
) {
    install_rln_ldk_peer_manager_hooks(RlnLdkPeerManagerHooks {
        new_outbound_connection: Rc::new(move |peer_pubkey| {
            let res = new_outbound_connection_cb
                .call1(&JsValue::NULL, &JsValue::from_str(peer_pubkey))?;
            res.as_string().ok_or_else(|| {
                JsValue::from_str(sdk_contracts::ERR_NEW_OUTBOUND_CONNECTION_CB_HEX_STRING)
            })
        }),
        read_event: Rc::new(move |_peer_pubkey, payload_hex| {
            let _ = read_event_cb.call1(&JsValue::NULL, &JsValue::from_str(payload_hex))?;
            Ok(())
        }),
        process_events: Rc::new(move || {
            let _ = process_events_cb.call0(&JsValue::NULL)?;
            Ok(())
        }),
        take_outbound_frames: Rc::new(move |_peer_pubkey| {
            let value = take_outbound_frames_cb.call0(&JsValue::NULL)?;
            if value.is_null() || value.is_undefined() {
                return Ok(Vec::new());
            }
            if !js_sys::Array::is_array(&value) {
                return Err(JsValue::from_str(
                    "take_outbound_frames callback must return string[]",
                ));
            }
            let array = js_sys::Array::from(&value);
            let mut frames = Vec::with_capacity(array.length() as usize);
            for item in array.iter() {
                let Some(frame) = item.as_string() else {
                    return Err(JsValue::from_str(
                        "take_outbound_frames callback array entries must be strings",
                    ));
                };
                if frame.trim().is_empty() {
                    continue;
                }
                frames.push(frame);
            }
            Ok(frames)
        }),
        socket_disconnected: Rc::new(move |_peer_pubkey| {
            let _ = socket_disconnected_cb.call0(&JsValue::NULL)?;
            Ok(())
        }),
        report_error: Rc::new(move |error_message| {
            let _ = report_error_cb.call1(&JsValue::NULL, &JsValue::from_str(error_message))?;
            Ok(())
        }),
    });
    RLN_LDK_PEER_MANAGER_HOOKS_V2_READY.with(|ready| ready.set(true));
}

#[wasm_bindgen(js_name = clearPeerManagerHooks)]
pub fn clear_peer_manager_hooks() {
    clear_rln_ldk_peer_manager_hooks();
}

#[wasm_bindgen(js_name = hasPeerManagerHooks)]
pub fn has_peer_manager_hooks() -> bool {
    get_rln_ldk_peer_manager_hooks().is_some()
}

#[wasm_bindgen(js_name = hasPeerManagerHooksV2)]
pub fn has_peer_manager_hooks_v2() -> bool {
    has_peer_manager_hooks() && RLN_LDK_PEER_MANAGER_HOOKS_V2_READY.with(|ready| ready.get())
}

struct RustPeerManagerAdapter {
    callbacks: RustPeerManagerCallbacks,
}

impl PeerManagerAdapter for RustPeerManagerAdapter {
    fn new_outbound_connection(&self, peer_pubkey: &str) -> Result<String, JsValue> {
        (self.callbacks.new_outbound_connection)(peer_pubkey)
    }

    fn read_event(&self, payload_hex: &str) -> Result<(), JsValue> {
        (self.callbacks.read_event)(payload_hex)
    }

    fn process_events(&self) -> Result<(), JsValue> {
        (self.callbacks.process_events)()
    }

    fn socket_disconnected(&self) -> Result<(), JsValue> {
        (self.callbacks.socket_disconnected)()
    }

    fn take_outbound_frames(&self) -> Result<Vec<String>, JsValue> {
        (self.callbacks.take_outbound_frames)()
    }

    fn report_error(&self, error_message: &str) -> Result<(), JsValue> {
        (self.callbacks.report_error)(error_message)
    }
}

fn callbacks_from_hooks(
    hooks: Rc<RlnLdkPeerManagerHooks>,
    peer_pubkey: String,
) -> RustPeerManagerCallbacks {
    RustPeerManagerCallbacks {
        new_outbound_connection: Box::new({
            let hooks = hooks.clone();
            move |peer_pubkey| (hooks.new_outbound_connection)(peer_pubkey)
        }),
        read_event: Box::new({
            let hooks = hooks.clone();
            let peer_pubkey = peer_pubkey.clone();
            move |payload_hex| (hooks.read_event)(&peer_pubkey, payload_hex)
        }),
        process_events: Box::new({
            let hooks = hooks.clone();
            move || (hooks.process_events)()
        }),
        socket_disconnected: Box::new({
            let hooks = hooks.clone();
            let peer_pubkey = peer_pubkey.clone();
            move || (hooks.socket_disconnected)(&peer_pubkey)
        }),
        take_outbound_frames: Box::new({
            let hooks = hooks.clone();
            let peer_pubkey = peer_pubkey.clone();
            move || (hooks.take_outbound_frames)(&peer_pubkey)
        }),
        report_error: Box::new(move |error_message| (hooks.report_error)(error_message)),
    }
}

enum PeerInboundMessage {
    PayloadHex {
        payload_hex: String,
        replay_seq: Option<u64>,
        replay_session_id: Option<String>,
    },
    TransportError(String),
}

fn drain_inbound_queue(
    adapter: &Rc<dyn PeerManagerAdapter>,
    inbound_queue: &Rc<RefCell<VecDeque<PeerInboundMessage>>>,
    disconnected: &Rc<Cell<bool>>,
    replay_cursor: Option<&Rc<Cell<u64>>>,
) -> Vec<String> {
    let mut outbound = Vec::new();
    loop {
        let inbound = {
            let mut q = inbound_queue.borrow_mut();
            q.pop_front()
        };
        let Some(inbound) = inbound else {
            break;
        };
        if disconnected.get() {
            break;
        }
        match inbound {
            PeerInboundMessage::TransportError(error) => {
                peer_session_debug(&format!(
                    "[rln-wasm-sdk peer-session] transport error={}",
                    error
                ));
                let _ = adapter.report_error(&error);
                let _ = adapter.socket_disconnected();
                let _ = adapter.process_events();
                disconnected.set(true);
                let _ = inbound_queue.borrow_mut().drain(..);
            }
            PeerInboundMessage::PayloadHex {
                payload_hex,
                replay_seq,
                replay_session_id,
            } => {
                peer_session_debug(&format!(
                    "[rln-wasm-sdk peer-session] inbound payload bytes={}",
                    payload_hex.len() / 2
                ));
                if let (Some(cursor), Some(seq)) = (replay_cursor, replay_seq) {
                    match replay_frame_disposition(cursor.get(), seq) {
                        ReplayFrameDisposition::DropDuplicate => {
                            peer_session_debug(&format!(
                                "[rln-wasm-sdk peer-session] dropping duplicate replay seq={seq} cursor={}",
                                cursor.get()
                            ));
                            continue;
                        }
                        ReplayFrameDisposition::Gap { expected_next } => {
                            let msg = format!(
                                "inbound replay sequence gap detected (expected {expected_next}, got {seq})"
                            );
                            peer_session_debug(&format!(
                                "[rln-wasm-sdk peer-session] transport error={msg}"
                            ));
                            let _ = adapter.report_error(&msg);
                            let _ = adapter.socket_disconnected();
                            let _ = adapter.process_events();
                            disconnected.set(true);
                            let _ = inbound_queue.borrow_mut().drain(..);
                            break;
                        }
                        ReplayFrameDisposition::Accept => {}
                    }
                }
                if let Err(err) = adapter.read_event(&payload_hex) {
                    let msg = err
                        .as_string()
                        .unwrap_or_else(|| "read_event callback failed".to_string());
                    peer_session_debug(&format!(
                        "[rln-wasm-sdk peer-session] read_event failed={}",
                        msg
                    ));
                    let _ = adapter.report_error(&msg);
                    let _ = adapter.socket_disconnected();
                    let _ = adapter.process_events();
                    disconnected.set(true);
                    let _ = inbound_queue.borrow_mut().drain(..);
                    continue;
                }
                if let Err(err) = adapter.process_events() {
                    let msg = err
                        .as_string()
                        .unwrap_or_else(|| "process_events callback failed".to_string());
                    peer_session_debug(&format!(
                        "[rln-wasm-sdk peer-session] process_events failed={}",
                        msg
                    ));
                    let _ = adapter.report_error(&msg);
                    let _ = adapter.socket_disconnected();
                    let _ = adapter.process_events();
                    disconnected.set(true);
                    let _ = inbound_queue.borrow_mut().drain(..);
                    continue;
                }
                if let Some(seq) = replay_seq {
                    if let Some(session_id) = replay_session_id.as_ref() {
                        commit_last_applied_seq(session_id, seq);
                    }
                    if let Some(cursor) = replay_cursor {
                        if seq > cursor.get() {
                            cursor.set(seq);
                        }
                    }
                }
                if let Ok(mut produced) = adapter.take_outbound_frames() {
                    outbound.append(&mut produced);
                }
            }
        }
    }
    outbound
}

#[wasm_bindgen]
pub struct RlnWasmPeerSession {
    socket: RlnWasmLnSocket,
    peer_pubkey: String,
    adapter: Rc<dyn PeerManagerAdapter>,
    started: Cell<bool>,
    read_loop_closure: RefCell<Option<Closure<dyn FnMut(JsValue)>>>,
}

#[wasm_bindgen]
impl RlnWasmPeerSession {
    #[wasm_bindgen(js_name = websocketUrl)]
    pub fn websocket_url(&self) -> String {
        self.socket.websocket_url()
    }

    #[wasm_bindgen(js_name = start)]
    pub async fn start(&self) -> Result<(), JsValue> {
        if self.started.get() {
            return Ok(());
        }
        peer_session_debug(&format!(
            "[rln-wasm-sdk peer-session] start peer_pubkey={} ws_url={}",
            self.peer_pubkey,
            self.socket.websocket_url()
        ));

        let initial_hex = self.adapter.new_outbound_connection(&self.peer_pubkey)?;
        let mut initial_outbound_frames = Vec::new();
        if !initial_hex.trim().is_empty() {
            initial_outbound_frames.push(initial_hex);
        }
        initial_outbound_frames.extend(self.adapter.take_outbound_frames()?);
        let initial_bytes: usize = initial_outbound_frames
            .iter()
            .map(|frame| frame.trim().len() / 2)
            .sum();
        peer_session_debug(&format!(
            "[rln-wasm-sdk peer-session] initial outbound frames={} bytes={}",
            initial_outbound_frames.len(),
            initial_bytes
        ));
        for frame in initial_outbound_frames {
            if frame.trim().is_empty() {
                continue;
            }
            self.socket.send_hex(frame).await?;
        }
        if initial_bytes > 0 {
            peer_session_debug("[rln-wasm-sdk peer-session] initial outbound sent");
        }

        let adapter = self.adapter.clone();
        let socket = self.socket.clone();
        let replay_cursor = self.socket.replay_applied_seq_cell();
        let replay_cursor_cb = replay_cursor.clone();
        let inbound_queue: Rc<RefCell<VecDeque<PeerInboundMessage>>> =
            Rc::new(RefCell::new(VecDeque::new()));
        let draining = Rc::new(Cell::new(false));
        let disconnected = Rc::new(Cell::new(false));
        let outbound_queue: Rc<RefCell<VecDeque<String>>> = Rc::new(RefCell::new(VecDeque::new()));
        let outbound_flush_scheduled = Rc::new(Cell::new(false));

        fn spawn_outbound_flush_task(
            outbound_queue: Rc<RefCell<VecDeque<String>>>,
            outbound_flush_scheduled: Rc<Cell<bool>>,
            disconnected: Rc<Cell<bool>>,
            socket: RlnWasmLnSocket,
        ) {
            spawn_local(async move {
                loop {
                    if disconnected.get() || socket.is_closed() {
                        break;
                    }

                    let next = outbound_queue.borrow_mut().pop_front();
                    let Some(frame) = next else {
                        break;
                    };

                    if frame.trim().is_empty() {
                        continue;
                    }

                    if let Err(err) = socket.send_hex(frame).await {
                        peer_session_debug(&format!(
                            "[rln-wasm-sdk peer-session] outbound send failed: {}",
                            err.as_string().unwrap_or_else(|| "unknown".to_string())
                        ));
                        disconnected.set(true);
                        break;
                    }
                }

                outbound_flush_scheduled.set(false);

                // While we were awaiting sends, JS may have queued more outbound frames.
                if !disconnected.get()
                    && !socket.is_closed()
                    && !outbound_queue.borrow().is_empty()
                    && !outbound_flush_scheduled.get()
                {
                    outbound_flush_scheduled.set(true);
                    spawn_outbound_flush_task(
                        outbound_queue,
                        outbound_flush_scheduled,
                        disconnected,
                        socket,
                    );
                }
            });
        }

        let schedule_outbound_flush: Rc<dyn Fn()> = {
            let outbound_queue = Rc::clone(&outbound_queue);
            let outbound_flush_scheduled = Rc::clone(&outbound_flush_scheduled);
            let disconnected = Rc::clone(&disconnected);
            let socket = socket.clone();

            Rc::new(move || {
                if disconnected.get() || socket.is_closed() || outbound_flush_scheduled.get() {
                    return;
                }
                outbound_flush_scheduled.set(true);
                spawn_outbound_flush_task(
                    Rc::clone(&outbound_queue),
                    Rc::clone(&outbound_flush_scheduled),
                    Rc::clone(&disconnected),
                    socket.clone(),
                );
            })
        };

        let enqueue_outbound_frames: Rc<dyn Fn(Vec<String>)> = {
            let outbound_queue = Rc::clone(&outbound_queue);
            let schedule_outbound_flush = Rc::clone(&schedule_outbound_flush);
            Rc::new(move |frames: Vec<String>| {
                if frames.is_empty() {
                    return;
                }
                outbound_queue.borrow_mut().extend(frames);
                schedule_outbound_flush();
            })
        };

        let disconnected_cb = Rc::clone(&disconnected);
        let enqueue_cb = Rc::clone(&enqueue_outbound_frames);

        let on_message = Closure::wrap(Box::new(move |message: JsValue| {
            if disconnected_cb.get() {
                return;
            }

            if let Some(error) = message.as_string() {
                inbound_queue
                    .borrow_mut()
                    .push_back(PeerInboundMessage::TransportError(error));
            } else {
                let maybe_replay: Result<RlnWasmReplayInboundFrameData, _> =
                    crate::js_from(message.clone());
                if let Ok(replay) = maybe_replay {
                    inbound_queue
                        .borrow_mut()
                        .push_back(PeerInboundMessage::PayloadHex {
                            payload_hex: replay.payload_hex,
                            replay_seq: Some(replay.seq),
                            replay_session_id: Some(replay.session_id),
                        });
                } else {
                    let array = js_sys::Uint8Array::new(&message);
                    let mut payload = vec![0u8; array.length() as usize];
                    array.copy_to(&mut payload);
                    let payload_hex = hex::encode(payload);
                    inbound_queue
                        .borrow_mut()
                        .push_back(PeerInboundMessage::PayloadHex {
                            payload_hex,
                            replay_seq: None,
                            replay_session_id: None,
                        });
                }
            }

            if draining.get() {
                return;
            }
            draining.set(true);
            let outbound = drain_inbound_queue(
                &adapter,
                &inbound_queue,
                &disconnected_cb,
                replay_cursor_cb.as_ref(),
            );
            enqueue_cb(outbound);
            draining.set(false);
        }) as Box<dyn FnMut(JsValue)>);
        let on_message_fn: Function = on_message.as_ref().unchecked_ref::<Function>().clone();

        self.socket.start_read_loop(on_message_fn)?;
        self.read_loop_closure.replace(Some(on_message));

        // Important: outbound frames can be generated by LDK without any inbound traffic
        // (e.g. immediately after a `keysend` call). If we only drain outbound frames in response
        // to inbound messages, payments can remain stuck in `pending` forever.
        //
        // This pump periodically:
        // - processes peer events, then
        // - drains outbound frames and pushes them over the websocket.
        //
        // The loop exits on socket close or disconnect.
        {
            let adapter = self.adapter.clone();
            let disconnected_pump = Rc::clone(&disconnected);
            let enqueue_pump = Rc::clone(&enqueue_outbound_frames);
            spawn_local(async move {
                loop {
                    if disconnected_pump.get() {
                        break;
                    }
                    // Best-effort; if anything fails we just retry next tick.
                    let _ = adapter.process_events();
                    if let Ok(outbound) = adapter.take_outbound_frames() {
                        enqueue_pump(outbound);
                    }
                    sleep_ms(50).await;
                }
            });
        }

        self.started.set(true);
        Ok(())
    }

    #[wasm_bindgen(js_name = stop)]
    pub fn stop(&self) {
        self.socket.stop_read_loop();
        self.read_loop_closure.replace(None);
        self.started.set(false);
    }

    #[wasm_bindgen(js_name = close)]
    pub async fn close(&self) -> Result<(), JsValue> {
        self.stop();
        self.socket.close().await?;
        self.adapter.socket_disconnected()?;
        Ok(())
    }

    #[wasm_bindgen(js_name = isStarted)]
    pub fn is_started(&self) -> bool {
        self.started.get()
    }
}

#[wasm_bindgen(js_name = peerSessionConnect)]
pub async fn peer_session_connect(
    proxy_url: String,
    peer_addr: String,
    peer_pubkey: String,
    new_outbound_connection_cb: Function,
    read_event_cb: Function,
    process_events_cb: Function,
    socket_disconnected_cb: Function,
) -> Result<RlnWasmPeerSession, JsValue> {
    let report_error_cb = Function::new_with_args(
        "message",
        "console.error('[rln-wasm-sdk peer-session]', message);",
    );
    peer_session_connect_with_error_cb(
        proxy_url,
        peer_addr,
        peer_pubkey,
        new_outbound_connection_cb,
        read_event_cb,
        process_events_cb,
        socket_disconnected_cb,
        report_error_cb,
    )
    .await
}

#[wasm_bindgen(js_name = peerSessionConnectWithOptions)]
pub async fn peer_session_connect_with_options(
    proxy_url: String,
    peer_addr: String,
    peer_pubkey: String,
    options_js: JsValue,
    new_outbound_connection_cb: Function,
    read_event_cb: Function,
    process_events_cb: Function,
    socket_disconnected_cb: Function,
) -> Result<RlnWasmPeerSession, JsValue> {
    let report_error_cb = Function::new_with_args(
        "message",
        "console.error('[rln-wasm-sdk peer-session]', message);",
    );
    peer_session_connect_with_options_and_error_cb(
        proxy_url,
        peer_addr,
        peer_pubkey,
        options_js,
        new_outbound_connection_cb,
        read_event_cb,
        process_events_cb,
        socket_disconnected_cb,
        report_error_cb,
    )
    .await
}

#[wasm_bindgen(js_name = peerSessionConnectWithErrorCb)]
pub async fn peer_session_connect_with_error_cb(
    proxy_url: String,
    peer_addr: String,
    peer_pubkey: String,
    new_outbound_connection_cb: Function,
    read_event_cb: Function,
    process_events_cb: Function,
    socket_disconnected_cb: Function,
    report_error_cb: Function,
) -> Result<RlnWasmPeerSession, JsValue> {
    let adapter = Rc::new(JsPeerManagerAdapter {
        new_outbound_connection_cb,
        read_event_cb,
        process_events_cb,
        socket_disconnected_cb,
        report_error_cb,
    });
    peer_session_connect_with_adapter(proxy_url, peer_addr, peer_pubkey, JsValue::NULL, adapter)
        .await
}

#[wasm_bindgen(js_name = peerSessionConnectWithOptionsAndErrorCb)]
pub async fn peer_session_connect_with_options_and_error_cb(
    proxy_url: String,
    peer_addr: String,
    peer_pubkey: String,
    options_js: JsValue,
    new_outbound_connection_cb: Function,
    read_event_cb: Function,
    process_events_cb: Function,
    socket_disconnected_cb: Function,
    report_error_cb: Function,
) -> Result<RlnWasmPeerSession, JsValue> {
    let adapter = Rc::new(JsPeerManagerAdapter {
        new_outbound_connection_cb,
        read_event_cb,
        process_events_cb,
        socket_disconnected_cb,
        report_error_cb,
    });
    peer_session_connect_with_adapter(proxy_url, peer_addr, peer_pubkey, options_js, adapter).await
}

pub async fn peer_session_connect_rust_callbacks(
    proxy_url: String,
    peer_addr: String,
    peer_pubkey: String,
    callbacks: RustPeerManagerCallbacks,
) -> Result<RlnWasmPeerSession, JsValue> {
    let adapter = Rc::new(RustPeerManagerAdapter { callbacks });
    peer_session_connect_with_adapter(proxy_url, peer_addr, peer_pubkey, JsValue::NULL, adapter)
        .await
}

#[derive(Default)]
struct RustPeerManagerState {
    initial_outbound_hex: String,
    received_frames: u32,
    processed_events: u32,
    disconnected: bool,
    last_error: Option<String>,
}

#[wasm_bindgen]
pub struct RlnWasmRustPeerManagerBridge {
    inner: Rc<RefCell<RustPeerManagerState>>,
}

#[wasm_bindgen]
impl RlnWasmRustPeerManagerBridge {
    #[wasm_bindgen(constructor)]
    pub fn new(
        initial_outbound_hex: Option<String>,
    ) -> Result<RlnWasmRustPeerManagerBridge, JsValue> {
        let initial_outbound_hex = initial_outbound_hex.unwrap_or_default();
        if !initial_outbound_hex.is_empty() {
            let _ = hex::decode(&initial_outbound_hex)
                .map_err(|e| JsValue::from_str(&format!("invalid initial_outbound_hex: {e}")))?;
        }
        Ok(Self {
            inner: Rc::new(RefCell::new(RustPeerManagerState {
                initial_outbound_hex,
                ..Default::default()
            })),
        })
    }

    #[wasm_bindgen(js_name = setInitialOutboundHex)]
    pub fn set_initial_outbound_hex(&self, initial_outbound_hex: String) -> Result<(), JsValue> {
        if !initial_outbound_hex.is_empty() {
            let _ = hex::decode(&initial_outbound_hex)
                .map_err(|e| JsValue::from_str(&format!("invalid initial_outbound_hex: {e}")))?;
        }
        self.inner.borrow_mut().initial_outbound_hex = initial_outbound_hex;
        Ok(())
    }

    #[wasm_bindgen(js_name = statsValue)]
    pub fn stats_value(&self) -> Result<JsValue, JsValue> {
        let s = self.inner.borrow();
        let obj = js_sys::Object::new();
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("initial_outbound_hex"),
            &JsValue::from_str(&s.initial_outbound_hex),
        )?;
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("received_frames"),
            &JsValue::from_f64(s.received_frames as f64),
        )?;
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("processed_events"),
            &JsValue::from_f64(s.processed_events as f64),
        )?;
        js_sys::Reflect::set(
            &obj,
            &JsValue::from_str("disconnected"),
            &JsValue::from_bool(s.disconnected),
        )?;
        let last_error = s
            .last_error
            .as_ref()
            .map(|v| JsValue::from_str(v))
            .unwrap_or(JsValue::NULL);
        js_sys::Reflect::set(&obj, &JsValue::from_str("last_error"), &last_error)?;
        Ok(obj.into())
    }

    #[wasm_bindgen(js_name = connectSession)]
    pub async fn connect_session(
        &self,
        proxy_url: String,
        peer_addr: String,
        peer_pubkey: String,
    ) -> Result<RlnWasmPeerSession, JsValue> {
        if let Some(hooks) = get_rln_ldk_peer_manager_hooks() {
            let callbacks = callbacks_from_hooks(hooks, peer_pubkey.clone());
            return peer_session_connect_rust_callbacks(
                proxy_url,
                peer_addr,
                peer_pubkey,
                callbacks,
            )
            .await;
        }

        let state = self.inner.clone();
        let callbacks = RustPeerManagerCallbacks {
            new_outbound_connection: Box::new(move |_peer_pubkey| {
                let hex = state.borrow().initial_outbound_hex.clone();
                if hex.trim().is_empty() {
                    return Err(JsValue::from_str(
                        "peer-manager bridge is not wired: install peer-manager hooks or provide initial outbound frame",
                    ));
                }
                Ok(hex)
            }),
            read_event: Box::new({
                let state = self.inner.clone();
                move |payload_hex| {
                    let _ = hex::decode(payload_hex)
                        .map_err(|e| JsValue::from_str(&format!("invalid payload_hex: {e}")))?;
                    state.borrow_mut().received_frames += 1;
                    Ok(())
                }
            }),
            process_events: Box::new({
                let state = self.inner.clone();
                move || {
                    state.borrow_mut().processed_events += 1;
                    Ok(())
                }
            }),
            socket_disconnected: Box::new({
                let state = self.inner.clone();
                move || {
                    state.borrow_mut().disconnected = true;
                    Ok(())
                }
            }),
            take_outbound_frames: Box::new(move || Ok(Vec::new())),
            report_error: Box::new({
                let state = self.inner.clone();
                move |msg| {
                    state.borrow_mut().last_error = Some(msg.to_string());
                    Ok(())
                }
            }),
        };

        peer_session_connect_rust_callbacks(proxy_url, peer_addr, peer_pubkey, callbacks).await
    }

    #[wasm_bindgen(js_name = connectSessionWithOptions)]
    pub async fn connect_session_with_options(
        &self,
        proxy_url: String,
        peer_addr: String,
        peer_pubkey: String,
        options_js: JsValue,
    ) -> Result<RlnWasmPeerSession, JsValue> {
        if let Some(hooks) = get_rln_ldk_peer_manager_hooks() {
            let callbacks = callbacks_from_hooks(hooks, peer_pubkey.clone());
            return peer_session_connect_with_adapter_rust_callbacks(
                proxy_url,
                peer_addr,
                peer_pubkey,
                options_js,
                callbacks,
            )
            .await;
        }

        let state = self.inner.clone();
        let callbacks = RustPeerManagerCallbacks {
            new_outbound_connection: Box::new(move |_peer_pubkey| {
                let hex = state.borrow().initial_outbound_hex.clone();
                if hex.trim().is_empty() {
                    return Err(JsValue::from_str(
                        "peer-manager bridge is not wired: install peer-manager hooks or provide initial outbound frame",
                    ));
                }
                Ok(hex)
            }),
            read_event: Box::new({
                let state = self.inner.clone();
                move |payload_hex| {
                    let _ = hex::decode(payload_hex)
                        .map_err(|e| JsValue::from_str(&format!("invalid payload_hex: {e}")))?;
                    state.borrow_mut().received_frames += 1;
                    Ok(())
                }
            }),
            process_events: Box::new({
                let state = self.inner.clone();
                move || {
                    state.borrow_mut().processed_events += 1;
                    Ok(())
                }
            }),
            socket_disconnected: Box::new({
                let state = self.inner.clone();
                move || {
                    state.borrow_mut().disconnected = true;
                    Ok(())
                }
            }),
            take_outbound_frames: Box::new(move || Ok(Vec::new())),
            report_error: Box::new({
                let state = self.inner.clone();
                move |msg| {
                    state.borrow_mut().last_error = Some(msg.to_string());
                    Ok(())
                }
            }),
        };

        peer_session_connect_with_adapter_rust_callbacks(
            proxy_url,
            peer_addr,
            peer_pubkey,
            options_js,
            callbacks,
        )
        .await
    }
}

async fn peer_session_connect_with_adapter(
    proxy_url: String,
    peer_addr: String,
    peer_pubkey: String,
    options_js: JsValue,
    adapter: Rc<dyn PeerManagerAdapter>,
) -> Result<RlnWasmPeerSession, JsValue> {
    let peer_pubkey = peer_pubkey.trim().to_string();
    if peer_pubkey.is_empty() {
        return Err(JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_EMPTY));
    }
    if SecpPublicKey::from_str(&peer_pubkey).is_err() {
        return Err(JsValue::from_str(sdk_contracts::ERR_PEER_PUBKEY_INVALID));
    }

    let socket = if options_js.is_null() || options_js.is_undefined() {
        ln_socket_connect(proxy_url, peer_addr).await?
    } else {
        ln_socket_connect_with_options(proxy_url, peer_addr, options_js).await?
    };
    Ok(RlnWasmPeerSession {
        socket,
        peer_pubkey,
        adapter,
        started: Cell::new(false),
        read_loop_closure: RefCell::new(None),
    })
}

async fn peer_session_connect_with_adapter_rust_callbacks(
    proxy_url: String,
    peer_addr: String,
    peer_pubkey: String,
    options_js: JsValue,
    callbacks: RustPeerManagerCallbacks,
) -> Result<RlnWasmPeerSession, JsValue> {
    let adapter = Rc::new(RustPeerManagerAdapter { callbacks });
    peer_session_connect_with_adapter(proxy_url, peer_addr, peer_pubkey, options_js, adapter).await
}
