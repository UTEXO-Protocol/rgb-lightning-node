use crate::entities::{channel_ids, channel_peer_data, mnemonic, prelude::*, revoked_token, rgb_config};
use crate::utils::{hex_str, hex_str_to_vec};
use lightning::ln::types::ChannelId;
use crate::error::APIError;
use bitcoin::secp256k1::PublicKey;
use migration::MigratorTrait;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectOptions, Database, DatabaseConnection,
    DeleteResult, EntityTrait, QueryFilter,
};
use std::collections::HashMap;
use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

pub struct DatabaseManager {
    db: DatabaseConnection,
    // Cache for RGB config to reduce database hits on frequent operations
    rgb_config_cache: Arc<Mutex<HashMap<String, Option<String>>>>,
}

impl DatabaseManager {
    pub async fn new(db_path: &Path) -> Result<Self, APIError> {
        tracing::info!("Initializing database at path: {}", db_path.display());
        let db_url = format!("sqlite://{}?mode=rwc", db_path.display());
        tracing::info!("Connecting to database URL: {}", db_url);
        let mut opt = ConnectOptions::new(db_url);
        opt.max_connections(10)
            .connect_timeout(Duration::from_secs(30));
        let db = Database::connect(opt)
            .await
            .map_err(|e| APIError::DatabaseError(e.to_string()))?;
        tracing::info!("Database connected successfully");

        tracing::info!("Running migrations");
        migration::Migrator::up(&db, None)
            .await
            .map_err(|e| APIError::DatabaseError(e.to_string()))?;
        tracing::info!("Migrations completed");

        Ok(Self {
            db,
            rgb_config_cache: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub async fn save_mnemonic(&self, encrypted_mnemonic: String) -> Result<(), APIError> {
        tracing::info!("Saving mnemonic to database");
        Mnemonic::delete_many()
            .exec(&self.db)
            .await
            .map_err(|e| APIError::DatabaseError(e.to_string()))?;

        let new_mnemonic = mnemonic::ActiveModel {
            id: ActiveValue::NotSet,
            encrypted_mnemonic: ActiveValue::Set(encrypted_mnemonic),
        };

        new_mnemonic
            .insert(&self.db)
            .await
            .map_err(|e| APIError::DatabaseError(e.to_string()))?;
        tracing::info!("Mnemonic saved successfully");

        Ok(())
    }

    pub async fn load_mnemonic(&self) -> Result<String, APIError> {
        tracing::info!("Loading mnemonic from database");
        let mnemonic_model = Mnemonic::find()
            .one(&self.db)
            .await
            .map_err(|e| APIError::DatabaseError(e.to_string()))?
            .ok_or(APIError::NotInitialized)?;
        tracing::info!("Mnemonic loaded successfully");

        Ok(mnemonic_model.encrypted_mnemonic)
    }

    pub async fn check_already_initialized(&self) -> Result<bool, APIError> {
        tracing::info!("Checking if already initialized");
        let mnemonics = Mnemonic::find()
            .all(&self.db)
            .await
            .map_err(|e| APIError::DatabaseError(e.to_string()))?;
        let initialized = !mnemonics.is_empty();
        tracing::info!("Already initialized: {}", initialized);
        Ok(initialized)
    }

    pub async fn save_channel_peer(
        &self,
        pubkey: &PublicKey,
        address: &SocketAddr,
    ) -> Result<(), APIError> {
        tracing::info!("Saving channel peer to database: {}@{}", pubkey, address);

        // Delete existing entry for this pubkey if it exists
        ChannelPeerData::delete_many()
            .filter(channel_peer_data::Column::PublicKey.eq(pubkey.to_string()))
            .exec(&self.db)
            .await
            .map_err(|e| APIError::DatabaseError(e.to_string()))?;

        // Insert new entry
        let new_peer = channel_peer_data::ActiveModel {
            id: ActiveValue::NotSet,
            public_key: ActiveValue::Set(pubkey.to_string()),
            socket_addr: ActiveValue::Set(address.to_string()),
        };

        new_peer
            .insert(&self.db)
            .await
            .map_err(|e| APIError::DatabaseError(e.to_string()))?;

        tracing::info!("Channel peer saved successfully");
        Ok(())
    }

    pub async fn load_channel_peers(&self) -> Result<HashMap<PublicKey, SocketAddr>, APIError> {
        tracing::debug!("Loading channel peers from database");

        let peers = ChannelPeerData::find()
            .all(&self.db)
            .await
            .map_err(|e| APIError::DatabaseError(e.to_string()))?;

        let mut peer_data = HashMap::new();
        for peer in peers {
            let pubkey: PublicKey = peer
                .public_key
                .parse()
                .map_err(|e| APIError::InvalidPeerInfo(format!("Invalid public key: {}", e)))?;
            let socket_addr: SocketAddr = peer
                .socket_addr
                .parse()
                .map_err(|e| APIError::InvalidPeerInfo(format!("Invalid socket address: {}", e)))?;
            peer_data.insert(pubkey, socket_addr);
        }

        tracing::debug!("Loaded {} channel peers from database", peer_data.len());
        Ok(peer_data)
    }

    pub async fn delete_channel_peer(&self, pubkey: &PublicKey) -> Result<(), APIError> {
        tracing::info!("Deleting channel peer from database: {}", pubkey);

        let result: DeleteResult = ChannelPeerData::delete_many()
            .filter(channel_peer_data::Column::PublicKey.eq(pubkey.to_string()))
            .exec(&self.db)
            .await
            .map_err(|e| APIError::DatabaseError(e.to_string()))?;

        if result.rows_affected > 0 {
            tracing::info!("Channel peer deleted successfully");
        } else {
            tracing::warn!("Channel peer not found for deletion: {}", pubkey);
        }

        Ok(())
    }

    pub async fn save_rgb_config(&self, key: &str, value: &str) -> Result<(), APIError> {
        tracing::info!("Saving RGB config to database: {} = {}", key, value);

        let existing = RgbConfig::find()
            .filter(rgb_config::Column::Key.eq(key))
            .one(&self.db)
            .await
            .map_err(|e| APIError::DatabaseError(e.to_string()))?;

        if let Some(model) = existing {
            let mut active_model: rgb_config::ActiveModel = model.into();
            active_model.value = ActiveValue::Set(value.to_string());
            active_model
                .update(&self.db)
                .await
                .map_err(|e| APIError::DatabaseError(e.to_string()))?;
        } else {
            let new_config = rgb_config::ActiveModel {
                id: ActiveValue::NotSet,
                key: ActiveValue::Set(key.to_string()),
                value: ActiveValue::Set(value.to_string()),
            };
            new_config
                .insert(&self.db)
                .await
                .map_err(|e| APIError::DatabaseError(e.to_string()))?;
        }

        // Update cache with new value
        self.rgb_config_cache
            .lock()
            .await
            .insert(key.to_string(), Some(value.to_string()));

        tracing::info!("RGB config saved successfully");
        Ok(())
    }

    pub async fn load_rgb_config(&self, key: &str) -> Result<Option<String>, APIError> {
        tracing::debug!("Loading RGB config from cache/database: {}", key);

        // Check cache first
        {
            let cache = self.rgb_config_cache.lock().await;
            if let Some(cached_value) = cache.get(key) {
                return Ok(cached_value.clone());
            }
        }

        // Load from database and cache it
        let config = RgbConfig::find()
            .filter(rgb_config::Column::Key.eq(key))
            .one(&self.db)
            .await
            .map_err(|e| APIError::DatabaseError(e.to_string()))?;

        let value = config.map(|c| c.value);
        {
            let mut cache = self.rgb_config_cache.lock().await;
            cache.insert(key.to_string(), value.clone());
        }

        Ok(value)
    }

    pub async fn migrate_indexer_url_from_file(&self, storage_dir: &Path) -> Result<(), APIError> {
        const INDEXER_URL_FNAME: &str = "indexer_url";

        let indexer_url_path = storage_dir.join(INDEXER_URL_FNAME);

        if !indexer_url_path.exists() {
            tracing::info!("No existing indexer_url file found, skipping migration");
            return Ok(());
        }

        tracing::info!("Found existing indexer_url file, migrating to database");

        let indexer_url = fs::read_to_string(&indexer_url_path)
            .map_err(APIError::IO)?
            .trim()
            .to_string();

        self.save_rgb_config("indexer_url", &indexer_url).await?;

        tracing::info!("Successfully migrated indexer_url from file to database");

        Ok(())
    }

    pub async fn migrate_bitcoin_network_from_file(&self, storage_dir: &Path) -> Result<(), APIError> {
        const BITCOIN_NETWORK_FNAME: &str = "bitcoin_network";

        let bitcoin_network_path = storage_dir.join(BITCOIN_NETWORK_FNAME);

        if !bitcoin_network_path.exists() {
            tracing::info!("No existing bitcoin_network file found, skipping migration");
            return Ok(());
        }

        tracing::info!("Found existing bitcoin_network file, migrating to database");

        let bitcoin_network = fs::read_to_string(&bitcoin_network_path)
            .map_err(APIError::IO)?
            .trim()
            .to_string();

        self.save_rgb_config("bitcoin_network", &bitcoin_network).await?;

        tracing::info!("Successfully migrated bitcoin_network from file to database");

        Ok(())
    }

    pub async fn migrate_wallet_fingerprint_from_file(&self, storage_dir: &Path) -> Result<(), APIError> {
        const WALLET_FINGERPRINT_FNAME: &str = "wallet_fingerprint";

        let wallet_fingerprint_path = storage_dir.join(WALLET_FINGERPRINT_FNAME);

        if !wallet_fingerprint_path.exists() {
            tracing::info!("No existing wallet_fingerprint file found, skipping migration");
            return Ok(());
        }

        tracing::info!("Found existing wallet_fingerprint file, migrating to database");

        let wallet_fingerprint = fs::read_to_string(&wallet_fingerprint_path)
            .map_err(APIError::IO)?
            .trim()
            .to_string();

        self.save_rgb_config("wallet_fingerprint", &wallet_fingerprint).await?;

        tracing::info!("Successfully migrated wallet_fingerprint from file to database");

        Ok(())
    }

    pub async fn migrate_wallet_account_xpub_colored_from_file(&self, storage_dir: &Path) -> Result<(), APIError> {
        const WALLET_ACCOUNT_XPUB_COLORED_FNAME: &str = "wallet_account_xpub_colored";

        let wallet_account_xpub_colored_path = storage_dir.join(WALLET_ACCOUNT_XPUB_COLORED_FNAME);

        if !wallet_account_xpub_colored_path.exists() {
            tracing::info!("No existing wallet_account_xpub_colored file found, skipping migration");
            return Ok(());
        }

        tracing::info!("Found existing wallet_account_xpub_colored file, migrating to database");

        let wallet_account_xpub_colored = fs::read_to_string(&wallet_account_xpub_colored_path)
            .map_err(APIError::IO)?
            .trim()
            .to_string();

        self.save_rgb_config("wallet_account_xpub_colored", &wallet_account_xpub_colored).await?;

        tracing::info!("Successfully migrated wallet_account_xpub_colored from file to database");

        Ok(())
    }

    pub async fn migrate_wallet_account_xpub_vanilla_from_file(&self, storage_dir: &Path) -> Result<(), APIError> {
        const WALLET_ACCOUNT_XPUB_VANILLA_FNAME: &str = "wallet_account_xpub_vanilla";

        let wallet_account_xpub_vanilla_path = storage_dir.join(WALLET_ACCOUNT_XPUB_VANILLA_FNAME);

        if !wallet_account_xpub_vanilla_path.exists() {
            tracing::info!("No existing wallet_account_xpub_vanilla file found, skipping migration");
            return Ok(());
        }

        tracing::info!("Found existing wallet_account_xpub_vanilla file, migrating to database");

        let wallet_account_xpub_vanilla = fs::read_to_string(&wallet_account_xpub_vanilla_path)
            .map_err(APIError::IO)?
            .trim()
            .to_string();

        self.save_rgb_config("wallet_account_xpub_vanilla", &wallet_account_xpub_vanilla).await?;

        tracing::info!("Successfully migrated wallet_account_xpub_vanilla from file to database");

        Ok(())
    }

    pub async fn migrate_wallet_master_fingerprint_from_file(&self, storage_dir: &Path) -> Result<(), APIError> {
        const WALLET_MASTER_FINGERPRINT_FNAME: &str = "wallet_master_fingerprint";

        let wallet_master_fingerprint_path = storage_dir.join(WALLET_MASTER_FINGERPRINT_FNAME);

        if !wallet_master_fingerprint_path.exists() {
            tracing::info!("No existing wallet_master_fingerprint file found, skipping migration");
            return Ok(());
        }

        tracing::info!("Found existing wallet_master_fingerprint file, migrating to database");

        let wallet_master_fingerprint = fs::read_to_string(&wallet_master_fingerprint_path)
            .map_err(APIError::IO)?
            .trim()
            .to_string();

        self.save_rgb_config("wallet_master_fingerprint", &wallet_master_fingerprint).await?;

        tracing::info!("Successfully migrated wallet_master_fingerprint from file to database");

        Ok(())
    }

    /// Saves a revoked token's revocation identifier to the database.
    /// The revocation_id is stored as a hex-encoded string.
    pub async fn save_revoked_token(&self, revocation_id_hex: &str) -> Result<(), APIError> {
        tracing::debug!("Saving revoked token to database: {}", revocation_id_hex);

        // Check if already exists to avoid duplicate key errors
        let existing = RevokedToken::find()
            .filter(revoked_token::Column::RevocationId.eq(revocation_id_hex))
            .one(&self.db)
            .await
            .map_err(|e| APIError::DatabaseError(e.to_string()))?;

        if existing.is_some() {
            tracing::debug!("Revocation ID already exists in database, skipping");
            return Ok(());
        }

        let new_revoked_token = revoked_token::ActiveModel {
            id: ActiveValue::NotSet,
            revocation_id: ActiveValue::Set(revocation_id_hex.to_string()),
        };

        new_revoked_token
            .insert(&self.db)
            .await
            .map_err(|e| APIError::DatabaseError(e.to_string()))?;

        tracing::debug!("Revoked token saved successfully");
        Ok(())
    }

    /// Loads all revoked token revocation identifiers from the database.
    /// Returns a HashSet of raw byte vectors (decoded from hex).
    pub async fn load_revoked_tokens(&self) -> Result<std::collections::HashSet<Vec<u8>>, APIError> {
        tracing::info!("Loading revoked tokens from database");

        let tokens = RevokedToken::find()
            .all(&self.db)
            .await
            .map_err(|e| APIError::DatabaseError(e.to_string()))?;

        let mut revoked: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
        for token in tokens {
            if let Some(bytes) = crate::utils::hex_str_to_vec(&token.revocation_id) {
                revoked.insert(bytes);
            } else {
                tracing::warn!(
                    "Invalid hex string in revoked_token table: {}",
                    token.revocation_id
                );
            }
        }

        tracing::info!("Loaded {} revoked tokens from database", revoked.len());
        Ok(revoked)
    }

    pub async fn save_channel_id(
        &self,
        temporary_channel_id: &ChannelId,
        channel_id: &ChannelId,
    ) -> Result<(), APIError> {
        let temp_id_hex = hex_str(&temporary_channel_id.0);
        let chan_id_hex = hex_str(&channel_id.0);
        tracing::debug!(
            "Saving channel ID mapping to database: {} -> {}",
            temp_id_hex,
            chan_id_hex
        );

        let existing = ChannelIds::find()
            .filter(channel_ids::Column::TemporaryChannelId.eq(&temp_id_hex))
            .one(&self.db)
            .await
            .map_err(|e| APIError::DatabaseError(e.to_string()))?;

        if let Some(model) = existing {
            let mut active_model: channel_ids::ActiveModel = model.into();
            active_model.channel_id = ActiveValue::Set(chan_id_hex);
            active_model
                .update(&self.db)
                .await
                .map_err(|e| APIError::DatabaseError(e.to_string()))?;
        } else {
            let new_entry = channel_ids::ActiveModel {
                id: ActiveValue::NotSet,
                temporary_channel_id: ActiveValue::Set(temp_id_hex),
                channel_id: ActiveValue::Set(chan_id_hex),
            };
            new_entry
                .insert(&self.db)
                .await
                .map_err(|e| APIError::DatabaseError(e.to_string()))?;
        }

        tracing::debug!("Channel ID mapping saved successfully");
        Ok(())
    }

    pub async fn load_channel_ids(&self) -> Result<HashMap<ChannelId, ChannelId>, APIError> {
        tracing::debug!("Loading channel IDs from database");

        let entries = ChannelIds::find()
            .all(&self.db)
            .await
            .map_err(|e| APIError::DatabaseError(e.to_string()))?;

        let mut channel_ids_map = HashMap::new();
        for entry in entries {
            let temp_id_bytes = match hex_str_to_vec(&entry.temporary_channel_id) {
                Some(bytes) => bytes,
                None => {
                    tracing::warn!(
                        "Invalid temporary_channel_id hex in database: {}",
                        entry.temporary_channel_id
                    );
                    continue;
                }
            };
            let chan_id_bytes = match hex_str_to_vec(&entry.channel_id) {
                Some(bytes) => bytes,
                None => {
                    tracing::warn!(
                        "Invalid channel_id hex in database: {}",
                        entry.channel_id
                    );
                    continue;
                }
            };

            if temp_id_bytes.len() != 32 || chan_id_bytes.len() != 32 {
                tracing::warn!(
                    "Invalid channel ID length in database: temp={}, chan={}",
                    temp_id_bytes.len(),
                    chan_id_bytes.len()
                );
                continue;
            }

            let temp_id = ChannelId(<[u8; 32]>::try_from(temp_id_bytes.as_slice()).unwrap());
            let chan_id = ChannelId(<[u8; 32]>::try_from(chan_id_bytes.as_slice()).unwrap());
            channel_ids_map.insert(temp_id, chan_id);
        }

        tracing::debug!("Loaded {} channel ID mappings from database", channel_ids_map.len());
        Ok(channel_ids_map)
    }

    pub async fn delete_channel_id_by_channel_id(
        &self,
        channel_id: &ChannelId,
    ) -> Result<(), APIError> {
        let chan_id_hex = hex_str(&channel_id.0);
        tracing::debug!("Deleting channel ID mapping by channel_id: {}", chan_id_hex);

        let result: DeleteResult = ChannelIds::delete_many()
            .filter(channel_ids::Column::ChannelId.eq(&chan_id_hex))
            .exec(&self.db)
            .await
            .map_err(|e| APIError::DatabaseError(e.to_string()))?;

        if result.rows_affected > 0 {
            tracing::debug!("Channel ID mapping deleted successfully");
        } else {
            tracing::debug!("No channel ID mapping found for deletion");
        }

        Ok(())
    }

    pub async fn migrate_channel_ids_from_file(
        &self,
        ldk_data_dir: &Path,
    ) -> Result<(), APIError> {
        use crate::disk::{read_channel_ids_info, CHANNEL_IDS_FNAME};

        let channel_ids_path = ldk_data_dir.join(CHANNEL_IDS_FNAME);

        if !channel_ids_path.exists() {
            tracing::info!("No existing channel_ids file found, skipping migration");
            return Ok(());
        }

        tracing::info!("Found existing channel_ids file, migrating to database");

        let channel_ids_map = read_channel_ids_info(&channel_ids_path);

        for (temp_id, chan_id) in channel_ids_map.channel_ids.iter() {
            self.save_channel_id(temp_id, chan_id).await?;
        }

        tracing::info!(
            "Successfully migrated {} channel ID mappings from file to database",
            channel_ids_map.channel_ids.len()
        );

        if let Err(e) = fs::remove_file(&channel_ids_path) {
            tracing::warn!("Failed to remove old channel_ids file: {}", e);
        } else {
            tracing::info!("Removed old channel_ids file after migration");
        }

        Ok(())
    }

    /// Syncs RGB config values from database to filesystem files.
    /// This is necessary because the rust-lightning library reads these values directly from files
    /// during RGB wallet operations (e.g., _get_indexer_url, _accept_transfer).
    /// The database is the source of truth, but files serve as a read-only cache for library compatibility.
    pub async fn sync_rgb_config_to_files(&self, storage_dir: &Path) -> Result<(), APIError> {
        const INDEXER_URL_FNAME: &str = "indexer_url";
        const PROXY_ENDPOINT_FNAME: &str = "proxy_endpoint";
        const BITCOIN_NETWORK_FNAME: &str = "bitcoin_network";
        const WALLET_FINGERPRINT_FNAME: &str = "wallet_fingerprint";
        const WALLET_ACCOUNT_XPUB_COLORED_FNAME: &str = "wallet_account_xpub_colored";
        const WALLET_ACCOUNT_XPUB_VANILLA_FNAME: &str = "wallet_account_xpub_vanilla";
        const WALLET_MASTER_FINGERPRINT_FNAME: &str = "wallet_master_fingerprint";

        let indexer_url = self.load_rgb_config("indexer_url").await?;
        let proxy_endpoint = self.load_rgb_config("proxy_endpoint").await?;
        let bitcoin_network = self.load_rgb_config("bitcoin_network").await?;
        let wallet_fingerprint = self.load_rgb_config("wallet_fingerprint").await?;
        let wallet_account_xpub_colored = self.load_rgb_config("wallet_account_xpub_colored").await?;
        let wallet_account_xpub_vanilla = self.load_rgb_config("wallet_account_xpub_vanilla").await?;
        let wallet_master_fingerprint = self.load_rgb_config("wallet_master_fingerprint").await?;

        if let Some(url) = indexer_url {
            let indexer_url_path = storage_dir.join(INDEXER_URL_FNAME);
            fs::write(&indexer_url_path, url).map_err(APIError::IO)?;
            tracing::info!("Synced indexer_url to file");
        }

        if let Some(proxy) = proxy_endpoint {
            let proxy_endpoint_path = storage_dir.join(PROXY_ENDPOINT_FNAME);
            fs::write(&proxy_endpoint_path, proxy).map_err(APIError::IO)?;
            tracing::info!("Synced proxy_endpoint to file");
        }

        if let Some(network) = bitcoin_network {
            let bitcoin_network_path = storage_dir.join(BITCOIN_NETWORK_FNAME);
            fs::write(&bitcoin_network_path, network).map_err(APIError::IO)?;
            tracing::info!("Synced bitcoin_network to file");
        }

        if let Some(fingerprint) = wallet_fingerprint {
            let wallet_fingerprint_path = storage_dir.join(WALLET_FINGERPRINT_FNAME);
            fs::write(&wallet_fingerprint_path, fingerprint).map_err(APIError::IO)?;
            tracing::info!("Synced wallet_fingerprint to file");
        }

        if let Some(xpub_colored) = wallet_account_xpub_colored {
            let wallet_account_xpub_colored_path = storage_dir.join(WALLET_ACCOUNT_XPUB_COLORED_FNAME);
            fs::write(&wallet_account_xpub_colored_path, xpub_colored).map_err(APIError::IO)?;
            tracing::info!("Synced wallet_account_xpub_colored to file");
        }

        if let Some(xpub_vanilla) = wallet_account_xpub_vanilla {
            let wallet_account_xpub_vanilla_path = storage_dir.join(WALLET_ACCOUNT_XPUB_VANILLA_FNAME);
            fs::write(&wallet_account_xpub_vanilla_path, xpub_vanilla).map_err(APIError::IO)?;
            tracing::info!("Synced wallet_account_xpub_vanilla to file");
        }

        if let Some(master_fingerprint) = wallet_master_fingerprint {
            let wallet_master_fingerprint_path = storage_dir.join(WALLET_MASTER_FINGERPRINT_FNAME);
            fs::write(&wallet_master_fingerprint_path, master_fingerprint).map_err(APIError::IO)?;
            tracing::info!("Synced wallet_master_fingerprint to file");
        }

        Ok(())
    }
}
