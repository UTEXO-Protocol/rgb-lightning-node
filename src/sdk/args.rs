use clap::{value_parser, Parser};
use rgb_lib::BitcoinNetwork;
use std::path::PathBuf;

use crate::sdk::error::AppError;
use crate::sdk::utils::{check_port_is_available, hex_str_to_vec};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path for the node storage directory
    storage_directory_path: PathBuf,

    /// Listening port of the daemon
    #[arg(long, default_value_t = 3001)]
    daemon_listening_port: u16,

    /// Listening port for LN peers
    #[arg(long, default_value_t = 9735)]
    ldk_peer_listening_port: u16,

    /// Bitcoin network
    #[arg(long, default_value_t = BitcoinNetwork::Testnet, value_parser = value_parser!(BitcoinNetwork))]
    network: BitcoinNetwork,

    /// Max allowed media size for upload (in MB)
    #[arg(long, default_value_t = 5)]
    max_media_upload_size_mb: u16,

    /// Root public key for biscuit token authentication (hex-encoded)
    #[arg(long)]
    root_public_key: Option<String>,

    /// Disable authentication
    #[arg(long, default_value_t = false)]
    disable_authentication: bool,
}

pub(crate) struct UserArgs {
    pub(crate) storage_dir_path: PathBuf,
    pub(crate) daemon_listening_port: u16,
    pub(crate) ldk_peer_listening_port: u16,
    pub(crate) network: BitcoinNetwork,
    pub(crate) max_media_upload_size_mb: u16,
    pub(crate) root_public_key: Option<biscuit_auth::PublicKey>,
}

fn check_auth_args(
    disable_authentication: bool,
    root_public_key: Option<String>,
) -> Result<Option<biscuit_auth::PublicKey>, AppError> {
    match (disable_authentication, root_public_key.is_some()) {
        (true, true) => return Err(AppError::InvalidAuthenticationArgs),
        (false, false) => return Err(AppError::InvalidAuthenticationArgs),
        _ => {}
    };

    Ok(if let Some(root_key_hex) = &root_public_key {
        let key_bytes = hex_str_to_vec(root_key_hex).ok_or(AppError::InvalidRootKey)?;
        if key_bytes.len() != 32 {
            return Err(AppError::InvalidRootKey);
        }
        let mut key_array = [0u8; 32];
        key_array.copy_from_slice(&key_bytes);
        let public_key =
            biscuit_auth::PublicKey::from_bytes(&key_array, biscuit_auth::Algorithm::Ed25519)
                .map_err(|_| AppError::InvalidRootKey)?;
        Some(public_key)
    } else {
        None
    })
}

pub(crate) fn parse_startup_args() -> Result<UserArgs, AppError> {
    let args = Args::parse();

    let network = args.network;

    let daemon_listening_port = args.daemon_listening_port;
    check_port_is_available(daemon_listening_port)?;
    let ldk_peer_listening_port = args.ldk_peer_listening_port;
    check_port_is_available(ldk_peer_listening_port)?;

    let root_public_key = check_auth_args(args.disable_authentication, args.root_public_key)?;

    Ok(UserArgs {
        storage_dir_path: args.storage_directory_path,
        daemon_listening_port,
        ldk_peer_listening_port,
        network,
        max_media_upload_size_mb: args.max_media_upload_size_mb,
        root_public_key,
    })
}
