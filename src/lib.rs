mod args;
mod auth;
mod backup;
mod bitcoind;
mod daemon;
mod disk;
mod error;
#[cfg(feature = "uniffi")]
pub mod ffi;
mod ldk;
mod rgb;
mod routes;
mod sdk;
mod swap;
#[cfg(feature = "uniffi")]
mod uniffi_api;
mod utils;

#[cfg(test)]
mod test;

#[cfg(test)]
use crate::{args::UserArgs, utils::LOGS_DIR};
#[cfg(test)]
use std::time::Duration;

pub use daemon::run_daemon;
#[cfg(test)]
pub(crate) use daemon::{app, shutdown_signal};

#[cfg(feature = "uniffi")]
pub use uniffi_api::*;
#[cfg(feature = "uniffi")]
pub(crate) use uniffi_api::{clear_uniffi_app_state, set_uniffi_app_state};
