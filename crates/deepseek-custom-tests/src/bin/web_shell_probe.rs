use deepseek_custom::web::server::start_production;

#[tokio::main]
async fn main() {
    let server = start_production(0, true, None)
        .await
        .expect("start embedded production web shell");
    println!("DeepSeekCustom production shell: {}", server.url());
    println!("Press Ctrl+C to stop.");
    tokio::signal::ctrl_c().await.expect("listen for Ctrl+C");
    server.shutdown().await.expect("stop production web shell");
}
