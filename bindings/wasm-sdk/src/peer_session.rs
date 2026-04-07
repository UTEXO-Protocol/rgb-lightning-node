use std::cell::{Cell, RefCell};
use std::rc::Rc;

use js_sys::Function;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use crate::ln_transport::{ln_socket_connect, RlnWasmLnSocket};

trait PeerManagerAdapter {
    fn new_outbound_connection(&self, peer_pubkey: &str) -> Result<String, JsValue>;
    fn read_event(&self, payload_hex: &str) -> Result<(), JsValue>;
    fn process_events(&self) -> Result<(), JsValue>;
    fn socket_disconnected(&self) -> Result<(), JsValue>;
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
            JsValue::from_str("new_outbound_connection callback must return a hex string")
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
    pub report_error: Box<dyn Fn(&str) -> Result<(), JsValue>>,
}

pub struct RlnLdkPeerManagerHooks {
    pub new_outbound_connection: Rc<dyn Fn(&str) -> Result<String, JsValue>>,
    pub read_event: Rc<dyn Fn(&str) -> Result<(), JsValue>>,
    pub process_events: Rc<dyn Fn() -> Result<(), JsValue>>,
    pub socket_disconnected: Rc<dyn Fn() -> Result<(), JsValue>>,
    pub report_error: Rc<dyn Fn(&str) -> Result<(), JsValue>>,
}

thread_local! {
    static RLN_LDK_PEER_MANAGER_HOOKS: RefCell<Option<Rc<RlnLdkPeerManagerHooks>>> = RefCell::new(None);
}

pub fn install_rln_ldk_peer_manager_hooks(hooks: RlnLdkPeerManagerHooks) {
    RLN_LDK_PEER_MANAGER_HOOKS.with(|slot| {
        slot.replace(Some(Rc::new(hooks)));
    });
}

pub fn clear_rln_ldk_peer_manager_hooks() {
    RLN_LDK_PEER_MANAGER_HOOKS.with(|slot| {
        slot.replace(None);
    });
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
    install_rln_ldk_peer_manager_hooks(RlnLdkPeerManagerHooks {
        new_outbound_connection: Rc::new(move |peer_pubkey| {
            let res = new_outbound_connection_cb
                .call1(&JsValue::NULL, &JsValue::from_str(peer_pubkey))?;
            res.as_string().ok_or_else(|| {
                JsValue::from_str("new_outbound_connection callback must return a hex string")
            })
        }),
        read_event: Rc::new(move |payload_hex| {
            let _ = read_event_cb.call1(&JsValue::NULL, &JsValue::from_str(payload_hex))?;
            Ok(())
        }),
        process_events: Rc::new(move || {
            let _ = process_events_cb.call0(&JsValue::NULL)?;
            Ok(())
        }),
        socket_disconnected: Rc::new(move || {
            let _ = socket_disconnected_cb.call0(&JsValue::NULL)?;
            Ok(())
        }),
        report_error: Rc::new(move |error_message| {
            let _ = report_error_cb.call1(&JsValue::NULL, &JsValue::from_str(error_message))?;
            Ok(())
        }),
    });
}

#[wasm_bindgen(js_name = clearPeerManagerHooks)]
pub fn clear_peer_manager_hooks() {
    clear_rln_ldk_peer_manager_hooks();
}

#[wasm_bindgen(js_name = hasPeerManagerHooks)]
pub fn has_peer_manager_hooks() -> bool {
    get_rln_ldk_peer_manager_hooks().is_some()
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

    fn report_error(&self, error_message: &str) -> Result<(), JsValue> {
        (self.callbacks.report_error)(error_message)
    }
}

fn callbacks_from_hooks(hooks: Rc<RlnLdkPeerManagerHooks>) -> RustPeerManagerCallbacks {
    RustPeerManagerCallbacks {
        new_outbound_connection: Box::new({
            let hooks = hooks.clone();
            move |peer_pubkey| (hooks.new_outbound_connection)(peer_pubkey)
        }),
        read_event: Box::new({
            let hooks = hooks.clone();
            move |payload_hex| (hooks.read_event)(payload_hex)
        }),
        process_events: Box::new({
            let hooks = hooks.clone();
            move || (hooks.process_events)()
        }),
        socket_disconnected: Box::new({
            let hooks = hooks.clone();
            move || (hooks.socket_disconnected)()
        }),
        report_error: Box::new(move |error_message| (hooks.report_error)(error_message)),
    }
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

        let initial_hex = self.adapter.new_outbound_connection(&self.peer_pubkey)?;
        if !initial_hex.trim().is_empty() {
            self.socket.send_hex(initial_hex).await?;
        }

        let adapter = self.adapter.clone();
        let on_message = Closure::wrap(Box::new(move |message: JsValue| {
            if message.as_string().is_some() {
                if let Some(error) = message.as_string() {
                    let _ = adapter.report_error(&error);
                }
                let _ = adapter.socket_disconnected();
                let _ = adapter.process_events();
                return;
            }

            let array = js_sys::Uint8Array::new(&message);
            let mut payload = vec![0u8; array.length() as usize];
            array.copy_to(&mut payload);
            let payload_hex = hex::encode(payload);

            if let Err(err) = adapter.read_event(&payload_hex) {
                let msg = err
                    .as_string()
                    .unwrap_or_else(|| "read_event callback failed".to_string());
                let _ = adapter.report_error(&msg);
                let _ = adapter.socket_disconnected();
                let _ = adapter.process_events();
                return;
            }
            if let Err(err) = adapter.process_events() {
                let msg = err
                    .as_string()
                    .unwrap_or_else(|| "process_events callback failed".to_string());
                let _ = adapter.report_error(&msg);
                let _ = adapter.socket_disconnected();
                let _ = adapter.process_events();
            }
        }) as Box<dyn FnMut(JsValue)>);
        let on_message_fn: Function = on_message.as_ref().unchecked_ref::<Function>().clone();

        self.socket.start_read_loop(on_message_fn)?;
        self.read_loop_closure.replace(Some(on_message));
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
    peer_session_connect_with_adapter(proxy_url, peer_addr, peer_pubkey, adapter).await
}

pub async fn peer_session_connect_rust_callbacks(
    proxy_url: String,
    peer_addr: String,
    peer_pubkey: String,
    callbacks: RustPeerManagerCallbacks,
) -> Result<RlnWasmPeerSession, JsValue> {
    let adapter = Rc::new(RustPeerManagerAdapter { callbacks });
    peer_session_connect_with_adapter(proxy_url, peer_addr, peer_pubkey, adapter).await
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
            let callbacks = callbacks_from_hooks(hooks);
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
}

async fn peer_session_connect_with_adapter(
    proxy_url: String,
    peer_addr: String,
    peer_pubkey: String,
    adapter: Rc<dyn PeerManagerAdapter>,
) -> Result<RlnWasmPeerSession, JsValue> {
    if peer_pubkey.trim().is_empty() {
        return Err(JsValue::from_str("peer_pubkey cannot be empty"));
    }

    let socket = ln_socket_connect(proxy_url, peer_addr).await?;
    Ok(RlnWasmPeerSession {
        socket,
        peer_pubkey,
        adapter,
        started: Cell::new(false),
        read_loop_closure: RefCell::new(None),
    })
}
