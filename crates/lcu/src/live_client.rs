use anyhow::{Context, bail};
use serde_json::Value;

const LIVE_CLIENT_DATA_URL: &str = "https://127.0.0.1:2999/liveclientdata/allgamedata";

pub async fn fetch_all_game_data() -> anyhow::Result<Value> {
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .no_proxy()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .context("failed to build Live Client Data HTTP client")?;

    let response = client
        .get(LIVE_CLIENT_DATA_URL)
        .send()
        .await
        .context("failed to connect to League Live Client Data API")?;

    let status = response.status();
    let text = response
        .text()
        .await
        .context("failed to read League Live Client Data response")?;

    if !status.is_success() {
        bail!("League Live Client Data API returned {status}: {text}");
    }

    serde_json::from_str(&text)
        .with_context(|| format!("failed to parse League Live Client Data JSON: {text}"))
}
