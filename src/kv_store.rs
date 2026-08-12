use std::sync::Arc;

use bitcoin::io;
use lightning::util::persist::KVStoreSync;
use sea_orm::sea_query::OnConflict;

use crate::database::entities::{KvStoreActMod, KvStoreColumn, KvStoreEntity};
use crate::runtime::block_on;
#[cfg(feature = "vss")]
use sea_orm::TransactionTrait;
use sea_orm::{ActiveValue, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};

#[cfg(test)]
fn kv_persistence_checkpoint(name: &str) {
    use std::fs::OpenOptions;
    use std::io::Write;

    if let Ok(path) = std::env::var("RLN_KV_PERSISTENCE_TRACE_PATH") {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open KV persistence trace");
        writeln!(file, "{name}").expect("write KV persistence trace");
        file.sync_all().expect("sync KV persistence trace");
    }

    if std::env::var("RLN_KV_KILL_AT").as_deref() == Ok(name) {
        let path = std::env::var("RLN_KV_KILL_READY_PATH").expect("KV persistence kill-ready path");
        let mut file = std::fs::File::create(path).expect("create KV persistence kill-ready file");
        writeln!(file, "{name}").expect("write KV persistence kill-ready file");
        file.sync_all()
            .expect("sync KV persistence kill-ready file");
        loop {
            std::thread::park();
        }
    }
}

#[cfg(not(test))]
#[inline]
fn kv_persistence_checkpoint(_name: &str) {}

/// sea-orm based KVStore implementation for LDK persistence.
pub struct SeaOrmKvStore {
    connection: Arc<DatabaseConnection>,
}

#[cfg(feature = "vss")]
pub(crate) struct KvStoreKey<'a> {
    primary_namespace: &'a str,
    secondary_namespace: &'a str,
    key: &'a str,
}

#[cfg(feature = "vss")]
impl<'a> KvStoreKey<'a> {
    pub(crate) fn new(
        primary_namespace: &'a str,
        secondary_namespace: &'a str,
        key: &'a str,
    ) -> Self {
        Self {
            primary_namespace,
            secondary_namespace,
            key,
        }
    }
}

#[cfg(feature = "vss")]
pub(crate) struct KvStoreEntry<'a> {
    key: KvStoreKey<'a>,
    value: Vec<u8>,
}

#[cfg(feature = "vss")]
impl<'a> KvStoreEntry<'a> {
    pub(crate) fn new(
        primary_namespace: &'a str,
        secondary_namespace: &'a str,
        key: &'a str,
        value: Vec<u8>,
    ) -> Self {
        Self {
            key: KvStoreKey::new(primary_namespace, secondary_namespace, key),
            value,
        }
    }

    fn into_active_model(self) -> KvStoreActMod {
        KvStoreActMod {
            primary_namespace: ActiveValue::Set(self.key.primary_namespace.to_string()),
            secondary_namespace: ActiveValue::Set(self.key.secondary_namespace.to_string()),
            key: ActiveValue::Set(self.key.key.to_string()),
            value: ActiveValue::Set(self.value),
        }
    }
}

impl SeaOrmKvStore {
    /// create a SeaOrmKvStore from an existing shared connection.
    /// does not run migrations (assumes they were already run).
    pub fn from_connection(connection: Arc<DatabaseConnection>) -> Self {
        Self { connection }
    }

    fn get_connection(&self) -> &DatabaseConnection {
        &self.connection
    }

    /// Atomically writes a local value and its durable VSS replication intent.
    ///
    /// A process may terminate immediately after this transaction commits. Keeping both rows in
    /// the same SQLite transaction guarantees that startup either observes the old local value
    /// with the old intent, or the new local value with the new intent; it can never observe a
    /// local mutation that has no remote-replay evidence.
    #[cfg(feature = "vss")]
    pub(crate) fn write_with_replication_intent(
        &self,
        value: KvStoreEntry<'_>,
        intent: KvStoreEntry<'_>,
    ) -> Result<(), io::Error> {
        let primary_namespace = value.key.primary_namespace;
        let secondary_namespace = value.key.secondary_namespace;
        let key = value.key.key;
        let value_model = value.into_active_model();
        let intent_model = intent.into_active_model();

        block_on(async {
            let transaction = self.get_connection().begin().await?;
            kv_persistence_checkpoint("atomic-write-before-value");
            KvStoreEntity::insert(value_model)
                .on_conflict(
                    OnConflict::columns([
                        KvStoreColumn::PrimaryNamespace,
                        KvStoreColumn::SecondaryNamespace,
                        KvStoreColumn::Key,
                    ])
                    .update_column(KvStoreColumn::Value)
                    .to_owned(),
                )
                .exec(&transaction)
                .await?;
            kv_persistence_checkpoint("atomic-write-after-value");
            kv_persistence_checkpoint("atomic-write-before-intent");
            KvStoreEntity::insert(intent_model)
                .on_conflict(
                    OnConflict::columns([
                        KvStoreColumn::PrimaryNamespace,
                        KvStoreColumn::SecondaryNamespace,
                        KvStoreColumn::Key,
                    ])
                    .update_column(KvStoreColumn::Value)
                    .to_owned(),
                )
                .exec(&transaction)
                .await?;
            kv_persistence_checkpoint("atomic-write-after-intent");
            kv_persistence_checkpoint("atomic-write-before-commit");
            transaction.commit().await?;
            kv_persistence_checkpoint("atomic-write-after-commit");
            Ok::<(), sea_orm::DbErr>(())
        })
        .map_err(|error| {
            tracing::error!(
                primary_namespace,
                secondary_namespace,
                key,
                error = %error,
                "Atomic KVStore write and replication-intent persistence failed"
            );
            io::Error::new(
                io::ErrorKind::Other,
                format!("Atomic database write and replication intent failed: {error}"),
            )
        })
    }

    /// Atomically removes a local value and records the corresponding VSS tombstone intent.
    #[cfg(feature = "vss")]
    pub(crate) fn remove_with_replication_intent(
        &self,
        key: KvStoreKey<'_>,
        intent: KvStoreEntry<'_>,
    ) -> Result<(), io::Error> {
        let primary_namespace = key.primary_namespace;
        let secondary_namespace = key.secondary_namespace;
        let key = key.key;
        let intent_model = intent.into_active_model();

        block_on(async {
            let transaction = self.get_connection().begin().await?;
            kv_persistence_checkpoint("atomic-remove-before-value");
            KvStoreEntity::delete_many()
                .filter(KvStoreColumn::PrimaryNamespace.eq(primary_namespace))
                .filter(KvStoreColumn::SecondaryNamespace.eq(secondary_namespace))
                .filter(KvStoreColumn::Key.eq(key))
                .exec(&transaction)
                .await?;
            kv_persistence_checkpoint("atomic-remove-after-value");
            kv_persistence_checkpoint("atomic-remove-before-intent");
            KvStoreEntity::insert(intent_model)
                .on_conflict(
                    OnConflict::columns([
                        KvStoreColumn::PrimaryNamespace,
                        KvStoreColumn::SecondaryNamespace,
                        KvStoreColumn::Key,
                    ])
                    .update_column(KvStoreColumn::Value)
                    .to_owned(),
                )
                .exec(&transaction)
                .await?;
            kv_persistence_checkpoint("atomic-remove-after-intent");
            kv_persistence_checkpoint("atomic-remove-before-commit");
            transaction.commit().await?;
            kv_persistence_checkpoint("atomic-remove-after-commit");
            Ok::<(), sea_orm::DbErr>(())
        })
        .map_err(|error| {
            tracing::error!(
                primary_namespace,
                secondary_namespace,
                key,
                error = %error,
                "Atomic KVStore removal and replication-intent persistence failed"
            );
            io::Error::new(
                io::ErrorKind::Other,
                format!("Atomic database removal and replication intent failed: {error}"),
            )
        })
    }
}

impl KVStoreSync for SeaOrmKvStore {
    fn read(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
    ) -> Result<Vec<u8>, io::Error> {
        tracing::trace!(primary_namespace, secondary_namespace, key, "KVStore read");

        let result = block_on(
            KvStoreEntity::find()
                .filter(KvStoreColumn::PrimaryNamespace.eq(primary_namespace))
                .filter(KvStoreColumn::SecondaryNamespace.eq(secondary_namespace))
                .filter(KvStoreColumn::Key.eq(key))
                .one(self.get_connection()),
        )
        .map_err(|e| {
            tracing::error!(
                primary_namespace,
                secondary_namespace,
                key,
                error = %e,
                "KVStore read failed"
            );
            io::Error::new(io::ErrorKind::Other, format!("Database read failed: {e}"))
        })?;

        match result {
            Some(record) => Ok(record.value),
            None => {
                tracing::trace!(
                    primary_namespace,
                    secondary_namespace,
                    key,
                    "KVStore key not found"
                );
                Err(io::Error::new(io::ErrorKind::NotFound, "Key not found"))
            }
        }
    }

    fn write(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        buf: Vec<u8>,
    ) -> Result<(), io::Error> {
        tracing::trace!(
            primary_namespace,
            secondary_namespace,
            key,
            value_len = buf.len(),
            "KVStore write"
        );

        let model = KvStoreActMod {
            primary_namespace: ActiveValue::Set(primary_namespace.to_string()),
            secondary_namespace: ActiveValue::Set(secondary_namespace.to_string()),
            key: ActiveValue::Set(key.to_string()),
            value: ActiveValue::Set(buf),
        };

        kv_persistence_checkpoint("plain-write-before-value");
        block_on(
            KvStoreEntity::insert(model)
                .on_conflict(
                    OnConflict::columns([
                        KvStoreColumn::PrimaryNamespace,
                        KvStoreColumn::SecondaryNamespace,
                        KvStoreColumn::Key,
                    ])
                    .update_column(KvStoreColumn::Value)
                    .to_owned(),
                )
                .exec(self.get_connection()),
        )
        .map_err(|e| {
            tracing::error!(
                primary_namespace,
                secondary_namespace,
                key,
                error = %e,
                "KVStore write failed"
            );
            io::Error::new(io::ErrorKind::Other, format!("Database write failed: {e}"))
        })?;
        kv_persistence_checkpoint("plain-write-after-value");

        Ok(())
    }

    fn remove(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        lazy: bool,
    ) -> Result<(), io::Error> {
        tracing::trace!(
            primary_namespace,
            secondary_namespace,
            key,
            lazy,
            "KVStore remove"
        );

        kv_persistence_checkpoint("plain-remove-before-value");
        block_on(
            KvStoreEntity::delete_many()
                .filter(KvStoreColumn::PrimaryNamespace.eq(primary_namespace))
                .filter(KvStoreColumn::SecondaryNamespace.eq(secondary_namespace))
                .filter(KvStoreColumn::Key.eq(key))
                .exec(self.get_connection()),
        )
        .map_err(|e| {
            tracing::error!(
                primary_namespace,
                secondary_namespace,
                key,
                error = %e,
                "KVStore remove failed"
            );
            io::Error::new(io::ErrorKind::Other, format!("Database delete failed: {e}"))
        })?;
        kv_persistence_checkpoint("plain-remove-after-value");

        Ok(())
    }

    fn list(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
    ) -> Result<Vec<String>, io::Error> {
        tracing::trace!(primary_namespace, secondary_namespace, "KVStore list");

        let results = block_on(
            KvStoreEntity::find()
                .filter(KvStoreColumn::PrimaryNamespace.eq(primary_namespace))
                .filter(KvStoreColumn::SecondaryNamespace.eq(secondary_namespace))
                .all(self.get_connection()),
        )
        .map_err(|e| {
            tracing::error!(
                primary_namespace,
                secondary_namespace,
                error = %e,
                "KVStore list failed"
            );
            io::Error::new(io::ErrorKind::Other, format!("Database list failed: {e}"))
        })?;

        let keys: Vec<String> = results.into_iter().map(|r| r.key).collect();
        Ok(keys)
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::{collections::HashMap, sync::Mutex};

    use bitcoin::io;
    use lightning::util::persist::KVStoreSync;

    #[derive(Default)]
    pub(crate) struct MemoryKvStore {
        entries: Mutex<HashMap<(String, String, String), Vec<u8>>>,
    }

    impl KVStoreSync for MemoryKvStore {
        fn read(
            &self,
            primary_namespace: &str,
            secondary_namespace: &str,
            key: &str,
        ) -> Result<Vec<u8>, io::Error> {
            self.entries
                .lock()
                .unwrap()
                .get(&(
                    primary_namespace.to_owned(),
                    secondary_namespace.to_owned(),
                    key.to_owned(),
                ))
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing key"))
        }

        fn write(
            &self,
            primary_namespace: &str,
            secondary_namespace: &str,
            key: &str,
            buf: Vec<u8>,
        ) -> Result<(), io::Error> {
            self.entries.lock().unwrap().insert(
                (
                    primary_namespace.to_owned(),
                    secondary_namespace.to_owned(),
                    key.to_owned(),
                ),
                buf,
            );
            Ok(())
        }

        fn remove(
            &self,
            primary_namespace: &str,
            secondary_namespace: &str,
            key: &str,
            _lazy: bool,
        ) -> Result<(), io::Error> {
            self.entries.lock().unwrap().remove(&(
                primary_namespace.to_owned(),
                secondary_namespace.to_owned(),
                key.to_owned(),
            ));
            Ok(())
        }

        fn list(
            &self,
            primary_namespace: &str,
            secondary_namespace: &str,
        ) -> Result<Vec<String>, io::Error> {
            Ok(self
                .entries
                .lock()
                .unwrap()
                .keys()
                .filter(|(primary, secondary, _)| {
                    primary == primary_namespace && secondary == secondary_namespace
                })
                .map(|(_, _, key)| key.clone())
                .collect())
        }
    }
}

#[cfg(all(test, feature = "vss"))]
mod persistence_tests {
    use super::*;
    use rln_migration::{Migrator, MigratorTrait};
    use sea_orm::{ConnectOptions, Database};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const TARGET_NS: &str = "crash-test";
    const TARGET_KEY: &str = "target";
    const INTENT_NS: &str = "crash-test-intent";
    const INTENT_KEY: &str = "target-intent";
    const OLD_VALUE: &[u8] = b"old-value";
    const NEW_VALUE: &[u8] = b"new-value";
    const WRITE_INTENT: &[u8] = b"write-intent";
    const REMOVE_INTENT: &[u8] = b"remove-intent";

    fn open_store(path: &Path) -> SeaOrmKvStore {
        let connection_string = format!("sqlite:{}?mode=rwc", path.display());
        let mut options = ConnectOptions::new(connection_string);
        options.max_connections(1).min_connections(1);
        let connection =
            crate::runtime::block_on(Database::connect(options)).expect("open test DB");
        crate::runtime::block_on(Migrator::up(&connection, None)).expect("migrate test DB");
        SeaOrmKvStore::from_connection(Arc::new(connection))
    }

    fn prepare_fixture(mode: &str, path: &Path) {
        let store = open_store(path);
        if matches!(
            mode,
            "atomic-write" | "atomic-remove" | "plain-write" | "plain-remove"
        ) {
            store
                .write(TARGET_NS, "", TARGET_KEY, OLD_VALUE.to_vec())
                .expect("seed target value");
        }
    }

    fn child_command(mode: &str, path: &Path) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("current test executable"));
        command
            .args([
                "--exact",
                "kv_store::persistence_tests::kv_store_os_kill_child",
                "--ignored",
                "--nocapture",
            ])
            .env("RLN_KV_CHILD_MODE", mode)
            .env("RLN_KV_CHILD_DB", path)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    fn trace_checkpoints(mode: &str) -> Vec<String> {
        let directory = tempfile::tempdir().expect("trace tempdir");
        let db_path = directory.path().join("store.sqlite");
        let trace_path = directory.path().join("trace.txt");
        prepare_fixture(mode, &db_path);
        let status = child_command(mode, &db_path)
            .env("RLN_KV_PERSISTENCE_TRACE_PATH", &trace_path)
            .status()
            .expect("run trace child");
        assert!(status.success(), "KV trace child failed for {mode}");
        let prefix = match mode {
            "atomic-write" => "atomic-write-",
            "atomic-remove" => "atomic-remove-",
            "plain-write" => "plain-write-",
            "plain-remove" => "plain-remove-",
            _ => unreachable!(),
        };
        let checkpoints: Vec<_> = std::fs::read_to_string(trace_path)
            .expect("read KV trace")
            .lines()
            .filter(|line| line.starts_with(prefix))
            .map(str::to_owned)
            .collect();
        assert!(
            !checkpoints.is_empty(),
            "no persistence checkpoints for {mode}"
        );
        let mut unique = checkpoints.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            checkpoints.len(),
            unique.len(),
            "duplicate checkpoints for {mode}"
        );
        checkpoints
    }

    fn kill_at_checkpoint(mode: &str, checkpoint: &str) -> PathBuf {
        let directory = tempfile::tempdir().expect("kill tempdir");
        let root = directory.keep();
        let db_path = root.join("store.sqlite");
        let ready_path = root.join("ready");
        prepare_fixture(mode, &db_path);
        let mut child = child_command(mode, &db_path)
            .env("RLN_KV_KILL_AT", checkpoint)
            .env("RLN_KV_KILL_READY_PATH", &ready_path)
            .spawn()
            .expect("spawn kill child");

        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if ready_path.exists() {
                break;
            }
            if let Some(status) = child.try_wait().expect("poll kill child") {
                panic!("KV child exited before {checkpoint}: {status}");
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("KV child did not reach {checkpoint}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.kill().expect("kill persistence child");
        let status = child.wait().expect("wait for persistence child");
        assert!(!status.success(), "killed child unexpectedly succeeded");
        db_path
    }

    fn optional_read(store: &SeaOrmKvStore, namespace: &str, key: &str) -> Option<Vec<u8>> {
        match store.read(namespace, "", key) {
            Ok(value) => Some(value),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => panic!("read failed: {error}"),
        }
    }

    fn assert_recovered_state(mode: &str, db_path: &Path) {
        let store = open_store(db_path);
        let target = optional_read(&store, TARGET_NS, TARGET_KEY);
        let intent = optional_read(&store, INTENT_NS, INTENT_KEY);
        match mode {
            "atomic-write" => assert!(
                (target.as_deref() == Some(OLD_VALUE) && intent.is_none())
                    || (target.as_deref() == Some(NEW_VALUE)
                        && intent.as_deref() == Some(WRITE_INTENT)),
                "atomic write recovered a split state: target={target:?}, intent={intent:?}"
            ),
            "atomic-remove" => assert!(
                (target.as_deref() == Some(OLD_VALUE) && intent.is_none())
                    || (target.is_none() && intent.as_deref() == Some(REMOVE_INTENT)),
                "atomic remove recovered a split state: target={target:?}, intent={intent:?}"
            ),
            "plain-write" => assert!(
                matches!(target.as_deref(), Some(OLD_VALUE) | Some(NEW_VALUE)),
                "plain write produced an invalid value: {target:?}"
            ),
            "plain-remove" => assert!(
                target.is_none() || target.as_deref() == Some(OLD_VALUE),
                "plain remove produced an invalid value: {target:?}"
            ),
            _ => unreachable!(),
        }
    }

    #[test]
    #[ignore = "subprocess used by kv_store_os_kill_matrix"]
    fn kv_store_os_kill_child() {
        let mode = std::env::var("RLN_KV_CHILD_MODE").expect("child mode");
        let db_path = PathBuf::from(std::env::var("RLN_KV_CHILD_DB").expect("child DB"));
        let store = open_store(&db_path);
        match mode.as_str() {
            "atomic-write" => store
                .write_with_replication_intent(
                    KvStoreEntry::new(TARGET_NS, "", TARGET_KEY, NEW_VALUE.to_vec()),
                    KvStoreEntry::new(INTENT_NS, "", INTENT_KEY, WRITE_INTENT.to_vec()),
                )
                .expect("atomic write"),
            "atomic-remove" => store
                .remove_with_replication_intent(
                    KvStoreKey::new(TARGET_NS, "", TARGET_KEY),
                    KvStoreEntry::new(INTENT_NS, "", INTENT_KEY, REMOVE_INTENT.to_vec()),
                )
                .expect("atomic remove"),
            "plain-write" => store
                .write(TARGET_NS, "", TARGET_KEY, NEW_VALUE.to_vec())
                .expect("plain write"),
            "plain-remove" => store
                .remove(TARGET_NS, "", TARGET_KEY, false)
                .expect("plain remove"),
            _ => panic!("unknown child mode {mode}"),
        }
    }

    #[test]
    fn kv_store_os_kill_matrix() {
        for mode in [
            "atomic-write",
            "atomic-remove",
            "plain-write",
            "plain-remove",
        ] {
            for checkpoint in trace_checkpoints(mode) {
                let db_path = kill_at_checkpoint(mode, &checkpoint);
                assert_recovered_state(mode, &db_path);
                std::fs::remove_dir_all(db_path.parent().expect("DB parent"))
                    .expect("remove kill fixture");
            }
        }
    }
}
