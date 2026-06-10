use tracing_subscriber::{EnvFilter, Layer, layer::SubscriberExt, util::SubscriberInitExt};

pub fn init() {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("symphony_vnext=info"));

    #[cfg(target_os = "linux")]
    {
        match tracing_journald::layer() {
            Ok(layer) => {
                if let Err(error) = tracing_subscriber::registry()
                    .with(env_filter)
                    .with(layer)
                    .try_init()
                {
                    eprintln!("symphony-vnext telemetry init skipped: {error}");
                }
                return;
            }
            Err(error) => {
                eprintln!("symphony-vnext telemetry journald init failed: {error}");
            }
        }
    }

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_writer(std::io::stderr)
        .boxed();
    if let Err(error) = tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .try_init()
    {
        eprintln!("symphony-vnext telemetry init skipped: {error}");
    }
}
