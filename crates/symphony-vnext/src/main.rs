#[tokio::main]
async fn main() -> anyhow::Result<()> {
    symphony_vnext::telemetry::init();
    symphony_vnext::cli::run().await
}
