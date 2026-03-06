use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    rgb_lightning_node::run_daemon().await
}
