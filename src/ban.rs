//! optional IP banning for juicehost. the ban list lives in an in-memory
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;

use crate::state::AppState;

#[derive(Debug, Error)]
enum BanSyncError {
    #[error("JUICEHOST_API_KEY not set (needed to authenticate the ban sync)")]
    MissingApiKey,
    #[error("failed to build HTTP client: {0}")]
    Client(#[source] reqwest::Error),
    #[error("request failed: {0}")]
    Request(#[source] reqwest::Error),
    #[error("backend returned status {0}")]
    Status(reqwest::StatusCode),
    #[error("invalid response: {0}")]
    InvalidResponse(#[source] reqwest::Error),
    #[error("backend returned an empty pepper")]
    EmptyPepper,
}

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
async fn sync_from_backend(state: &Arc<AppState>, url: &str) -> Result<(), BanSyncError> {
    if state.api_key.is_empty() {
        return Err(BanSyncError::MissingApiKey);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(BanSyncError::Client)?;

    let resp = client
        .get(format!("{url}/internal/ban-snapshot"))
        .header("x-juicehost-api-key", &state.api_key)
        .send()
        .await
        .map_err(BanSyncError::Request)?;

    if !resp.status().is_success() {
        return Err(BanSyncError::Status(resp.status()));
    }

    let body: serde_json::Value = resp.json().await.map_err(BanSyncError::InvalidResponse)?;

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
        return Err(BanSyncError::EmptyPepper);
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
