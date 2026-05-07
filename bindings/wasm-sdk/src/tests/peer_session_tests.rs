use secp256k1::PublicKey as SecpPublicKey;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use crate::ln_transport::commit_last_applied_seq;
use std::str::FromStr;
use wasm_bindgen::JsValue;

#[test]
fn peer_pubkey_validation_contract() {
    let valid = "0334cc4bca04ce3d1537310f55e91ec4cec7e5a88fa0fba20a24cce1fe6de2a2b0".to_string();
    assert!(SecpPublicKey::from_str(&valid).is_ok());
    assert!(SecpPublicKey::from_str("not-a-pubkey").is_err());
}

struct TestAdapter {
    process_calls: Rc<Cell<u32>>,
    outbound: Rc<RefCell<Vec<String>>>,
}

impl super::PeerManagerAdapter for TestAdapter {
    fn new_outbound_connection(&self, _peer_pubkey: &str) -> Result<String, JsValue> {
        Ok(String::new())
    }

    fn read_event(&self, _payload_hex: &str) -> Result<(), JsValue> {
        Ok(())
    }

    fn process_events(&self) -> Result<(), JsValue> {
        self.process_calls
            .set(self.process_calls.get().saturating_add(1));
        Ok(())
    }

    fn socket_disconnected(&self) -> Result<(), JsValue> {
        Ok(())
    }

    fn take_outbound_frames(&self) -> Result<Vec<String>, JsValue> {
        Ok(std::mem::take(&mut *self.outbound.borrow_mut()))
    }

    fn report_error(&self, _error_message: &str) -> Result<(), JsValue> {
        Ok(())
    }
}

#[test]
fn drain_inbound_queue_returns_outbound_frames_contract() {
    let process_calls = Rc::new(Cell::new(0));
    let outbound = Rc::new(RefCell::new(vec!["aa".to_string(), "bb".to_string()]));
    let adapter: Rc<dyn super::PeerManagerAdapter> = Rc::new(TestAdapter {
        process_calls: Rc::clone(&process_calls),
        outbound: Rc::clone(&outbound),
    });
    let inbound_queue = Rc::new(RefCell::new(VecDeque::new()));
    inbound_queue
        .borrow_mut()
        .push_back(super::PeerInboundMessage::PayloadHex {
            payload_hex: "0102".to_string(),
            replay_seq: None,
            replay_session_id: None,
        });
    let disconnected = Rc::new(Cell::new(false));

    let produced = super::drain_inbound_queue(&adapter, &inbound_queue, &disconnected, None);

    assert_eq!(produced, vec!["aa".to_string(), "bb".to_string()]);
    assert_eq!(process_calls.get(), 1);
    assert!(inbound_queue.borrow().is_empty());
    assert!(outbound.borrow().is_empty());
}

#[test]
fn drain_inbound_queue_drops_duplicate_replay_seq_when_cursor_present() {
    let process_calls = Rc::new(Cell::new(0));
    let outbound = Rc::new(RefCell::new(Vec::new()));
    let adapter: Rc<dyn super::PeerManagerAdapter> = Rc::new(TestAdapter {
        process_calls: Rc::clone(&process_calls),
        outbound: Rc::clone(&outbound),
    });
    let inbound_queue = Rc::new(RefCell::new(VecDeque::new()));
    let disconnected = Rc::new(Cell::new(false));
    let cursor = Rc::new(Cell::new(1u64));

    inbound_queue
        .borrow_mut()
        .push_back(super::PeerInboundMessage::PayloadHex {
            payload_hex: "0102".to_string(),
            replay_seq: Some(2),
            replay_session_id: Some("sess-a".to_string()),
        });
    inbound_queue
        .borrow_mut()
        .push_back(super::PeerInboundMessage::PayloadHex {
            payload_hex: "0102".to_string(),
            replay_seq: Some(2),
            replay_session_id: Some("sess-a".to_string()),
        });

    let produced =
        super::drain_inbound_queue(&adapter, &inbound_queue, &disconnected, Some(&cursor));

    assert!(produced.is_empty());
    assert_eq!(process_calls.get(), 1);
    assert_eq!(cursor.get(), 2);
    assert!(inbound_queue.borrow().is_empty());
    assert!(!disconnected.get());
}

#[test]
fn drain_inbound_queue_gap_replay_seq_disconnects_when_cursor_present() {
    let process_calls = Rc::new(Cell::new(0));
    let outbound = Rc::new(RefCell::new(Vec::new()));
    let adapter: Rc<dyn super::PeerManagerAdapter> = Rc::new(TestAdapter {
        process_calls: Rc::clone(&process_calls),
        outbound: Rc::clone(&outbound),
    });
    let inbound_queue = Rc::new(RefCell::new(VecDeque::new()));
    let disconnected = Rc::new(Cell::new(false));
    let cursor = Rc::new(Cell::new(1u64));

    inbound_queue
        .borrow_mut()
        .push_back(super::PeerInboundMessage::PayloadHex {
            payload_hex: "aa".to_string(),
            replay_seq: Some(5),
            replay_session_id: Some("sess-gap".to_string()),
        });

    let produced =
        super::drain_inbound_queue(&adapter, &inbound_queue, &disconnected, Some(&cursor));

    assert!(produced.is_empty());
    assert_eq!(process_calls.get(), 1);
    assert!(disconnected.get());
    assert_eq!(cursor.get(), 1);
    assert!(inbound_queue.borrow().is_empty());
}

#[test]
fn commit_last_applied_seq_is_noop_for_empty_session_id() {
    commit_last_applied_seq("", 9);
}
