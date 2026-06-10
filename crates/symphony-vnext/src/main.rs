#[tokio::main]
async fn main() -> anyhow::Result<()> {
    symphony_vnext::cli::run().await
}
