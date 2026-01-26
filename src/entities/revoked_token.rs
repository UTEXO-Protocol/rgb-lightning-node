//! `SeaORM` Entity for revoked token storage.
//! This table stores revocation identifiers for revoked Biscuit tokens.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "revoked_token")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub revocation_id: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
