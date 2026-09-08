//! Installs the Chromium build matched to the locked `playwright-rs` driver.

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "Installing Chromium for Playwright {}",
        playwright_rs::PLAYWRIGHT_VERSION
    );
    playwright_rs::install_browsers(Some(&["chromium"])).await?;
    Ok(())
}
