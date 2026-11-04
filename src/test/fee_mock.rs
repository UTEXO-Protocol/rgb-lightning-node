use std::collections::HashMap;
use std::sync::Mutex;

use lightning::chain::chaininterface::ConfirmationTarget;

lazy_static::lazy_static! {
    static ref MOCK_FEE: Mutex<Option<u32>> = Mutex::new(None);
    // persistent per-target overrides used by tests that need a stable fee estimate
    static ref TARGET_FEES: Mutex<HashMap<ConfirmationTarget, u32>> = Mutex::new(HashMap::new());
}

pub(crate) fn mock_fee(target: ConfirmationTarget, fee: u32) -> u32 {
    if let Some(fee) = TARGET_FEES.lock().unwrap().get(&target) {
        return *fee;
    }
    let mock = MOCK_FEE.lock().unwrap().take();
    if let Some(fee) = mock {
        println!("mocking fee");
        fee
    } else {
        fee
    }
}

pub(crate) fn set_mock_fee_for_tests(fee: Option<u32>) {
    *MOCK_FEE.lock().unwrap() = fee;
}

pub(crate) fn set_target_fee_for_tests(target: ConfirmationTarget, fee: Option<u32>) {
    let mut fees = TARGET_FEES.lock().unwrap();
    match fee {
        Some(fee) => {
            fees.insert(target, fee);
        }
        None => {
            fees.remove(&target);
        }
    }
}
