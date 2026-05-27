use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};

#[tokio::main]
async fn main() {
    fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    info!("DeepSeekCustom harness starting");

    // Spawn background tasks or hold main loop here (placeholder for agent/TUI init)
    tokio::signal::ctrl_c().await.ok();
    info!("DeepSeekCustom harness shutting down");
}
