use std::fs;
use std::path::Path;
use std::sync::Mutex;

use entity::mnemonic;
use magic_crypt::{new_magic_crypt, MagicCryptTrait};
use rgb_lib::bdk_wallet::keys::bip39::Mnemonic;
use rusqlite::Connection;
use sea_query::{ColumnDef, Expr, Query, SqliteQueryBuilder, Table};
use std::str::FromStr;

use crate::error::APIError;

const RLN_DB_NAME: &str = "rln_db";

/// Thread-safe wrapper around rusqlite Connection
pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    fn new(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
        }
    }

    fn with_connection<F, T>(&self, f: F) -> Result<T, APIError>
    where
        F: FnOnce(&Connection) -> Result<T, APIError>,
    {
        let conn = self
            .conn
            .lock()
            .map_err(|e| APIError::Unexpected(format!("Failed to acquire database lock: {e}")))?;
        f(&conn)
    }
}

pub fn init_db(storage_dir_path: &Path) -> Result<Database, APIError> {
    let db_path = storage_dir_path.join(RLN_DB_NAME);

    let conn = Connection::open(&db_path)
        .map_err(|e| APIError::Unexpected(format!("Failed to open database: {e}")))?;

    let create_table = Table::create()
        .table(mnemonic::Entity)
        .if_not_exists()
        .col(
            ColumnDef::new(mnemonic::Column::Id)
                .integer()
                .not_null()
                .auto_increment()
                .primary_key(),
        )
        .col(
            ColumnDef::new(mnemonic::Column::EncryptedMnemonic)
                .string()
                .not_null(),
        )
        .build(SqliteQueryBuilder);

    conn.execute(&create_table, [])
        .map_err(|e| APIError::Unexpected(format!("Failed to create mnemonic table: {e}")))?;

    Ok(Database::new(conn))
}

pub fn is_initialized(db: &Database) -> Result<bool, APIError> {
    db.with_connection(|conn| is_initialized_inner(conn))
}

fn is_initialized_inner(conn: &Connection) -> Result<bool, APIError> {
    let sql = Query::select()
        .expr(Expr::col(mnemonic::Column::Id))
        .from(mnemonic::Entity)
        .and_where(Expr::col(mnemonic::Column::Id).eq(1))
        .to_string(SqliteQueryBuilder);

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| APIError::Unexpected(format!("Database error: {e}")))?;

    let exists = stmt
        .exists([])
        .map_err(|e| APIError::Unexpected(format!("Database error: {e}")))?;

    Ok(exists)
}

pub fn check_already_initialized(db: &Database) -> Result<(), APIError> {
    if is_initialized(db)? {
        return Err(APIError::AlreadyInitialized);
    }
    Ok(())
}

pub fn save_encrypted_mnemonic(
    db: &Database,
    password: &str,
    mnemonic_str: &str,
) -> Result<(), APIError> {
    let mcrypt = new_magic_crypt!(password, 256);
    let encrypted_mnemonic = mcrypt.encrypt_str_to_base64(mnemonic_str);

    db.with_connection(|conn| {
        if is_initialized_inner(conn)? {
            let sql = Query::update()
                .table(mnemonic::Entity)
                .value(mnemonic::Column::EncryptedMnemonic, &encrypted_mnemonic)
                .and_where(Expr::col(mnemonic::Column::Id).eq(1))
                .to_string(SqliteQueryBuilder);

            conn.execute(&sql, [])
                .map_err(|e| APIError::Unexpected(format!("Failed to update mnemonic: {e}")))?;
        } else {
            let sql = Query::insert()
                .into_table(mnemonic::Entity)
                .columns([mnemonic::Column::EncryptedMnemonic])
                .values_panic([encrypted_mnemonic.into()])
                .to_string(SqliteQueryBuilder);

            conn.execute(&sql, [])
                .map_err(|e| APIError::Unexpected(format!("Failed to save mnemonic: {e}")))?;

            tracing::info!("Created a new wallet");
        }

        Ok(())
    })
}

pub fn get_mnemonic(db: &Database, password: &str) -> Result<Mnemonic, APIError> {
    db.with_connection(|conn| {
        let sql = Query::select()
            .column(mnemonic::Column::EncryptedMnemonic)
            .from(mnemonic::Entity)
            .and_where(Expr::col(mnemonic::Column::Id).eq(1))
            .to_string(SqliteQueryBuilder);

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| APIError::Unexpected(format!("Database error: {e}")))?;

        let encrypted_mnemonic: String =
            stmt.query_row([], |row| row.get(0)).map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => APIError::NotInitialized,
                _ => APIError::Unexpected(format!("Database error: {e}")),
            })?;

        let mcrypt = new_magic_crypt!(password, 256);
        let mnemonic_str = mcrypt
            .decrypt_base64_to_string(&encrypted_mnemonic)
            .map_err(|_| APIError::WrongPassword)?;

        Ok(Mnemonic::from_str(&mnemonic_str).expect("valid mnemonic"))
    })
}

/// Migrates mnemonic from legacy file storage to database.
/// This is used during restore operations when the backup contains a file-based mnemonic.
pub fn migrate_mnemonic_from_file(
    db: &Database,
    storage_dir_path: &Path,
    password: &str,
) -> Result<Mnemonic, APIError> {
    let mnemonic_path = storage_dir_path.join("mnemonic");

    let encrypted_mnemonic =
        fs::read_to_string(&mnemonic_path).map_err(|_| APIError::NotInitialized)?;

    let mcrypt = new_magic_crypt!(password, 256);
    let mnemonic_str = mcrypt
        .decrypt_base64_to_string(&encrypted_mnemonic)
        .map_err(|_| APIError::WrongPassword)?;

    let mnemonic = Mnemonic::from_str(&mnemonic_str).expect("valid mnemonic");
    save_encrypted_mnemonic(db, password, &mnemonic.to_string())?;
    let _ = fs::remove_file(&mnemonic_path);

    Ok(mnemonic)
}
