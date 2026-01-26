use sea_orm_migration::{prelude::*, schema::*};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(RevokedToken::Table)
                    .if_not_exists()
                    .col(pk_auto(RevokedToken::Id))
                    .col(string(RevokedToken::RevocationId).unique_key())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(RevokedToken::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum RevokedToken {
    Table,
    Id,
    RevocationId,
}
