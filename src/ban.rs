//! optional IP banning for juicehost. the ban list lives in an in-memory
use std::sync::Arc;
use std::time::Duration;

use crate::state::AppState;

pub async fn refresh_ban_list(state: &Arc<AppState>) {
    if let Some(ref url) = state.ban_sync_url {
        if let Err(e) = sync_from_backend(state, url).await {
            tracing::warn!("ban list sync from {} failed: {e}", url);
        }
    }
    let pepper = state.ban_list.pepper();
    if let Some(ref path) = state.ban_list_file {
        state.ban_list.load_file(path, &pepper);
    }
}

/// Pull the latest ban hashes + pepper from a juiceback backend.
/// There is probably a better way to do this, considering juicehost is publicly hostable.
async fn sync_from_backend(state: &Arc<AppState>, url: &str) -> Result<(), String> {
    if state.api_key.is_empty() {
        return Err("JUICEHOST_API_KEY not set (needed to authenticate the ban sync)".into());
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("failed to build http client: {e}"))?;

    let resp = client
        .get(format!("{url}/internal/ban-snapshot"))
        .header("x-juicehost-api-key", &state.api_key)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("backend returned status {}", resp.status()));
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("invalid response: {e}"))?;

    let pepper = body
        .get("pepper")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let hashes: Vec<String> = body
        .get("hashes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|h| h.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    if pepper.is_empty() {
        return Err("backend returned an empty pepper".into());
    }

    state.ban_list.set_snapshot(&pepper, hashes);
    tracing::info!("synced {} bans from {}", state.ban_list.len(), url);
    Ok(())
}

/// Loop that keeps the ban list fresh...
/// It is spawned at startup.
pub async fn ban_refresh_loop(state: Arc<AppState>) {
    let interval = state.ban_sync_interval.max(5);

    // Populate immediately so bans are enforced from the first request.
    refresh_ban_list(&state).await;

    let mut ticker = tokio::time::interval(Duration::from_secs(interval));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        ticker.tick().await;
        refresh_ban_list(&state).await;
    }
}
