use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};

fn main() {
    fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    info!("DeepSeekCustom harness starting");
}
