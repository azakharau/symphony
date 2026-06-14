#[tokio::main]
async fn main() -> anyhow::Result<()> {
    symphony::telemetry::init();
    symphony::cli::run().await
}
