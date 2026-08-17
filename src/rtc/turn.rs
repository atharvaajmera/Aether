//! Shared TURN credential fetch against the relay's `/turn-credentials`
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use crate::rtc::resolve::{RELAY_FALLBACK_IP, RELAY_HOST, fallback_applies};
use crate::signaling::IceServer;

const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(serde::Deserialize)]
struct TurnCredentialsResponse {
    username: String,
    credential: String,
    urls: Vec<String>,
}

fn turn_credentials_url(relay_server_url: &str, peer_id: &str) -> Option<reqwest::Url> {
    let trimmed = relay_server_url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut url = reqwest::Url::parse(trimmed).ok()?;
    let https_scheme = match url.scheme() {
        "wss" | "https" => "https",
        "ws" | "http" => "http",
        _ => "https",
    };
    url.set_scheme(https_scheme).ok()?;
    url.set_path("/turn-credentials");
    url.set_query(Some(&format!("peer_id={peer_id}")));
    Some(url)
}

async fn fetch_with_client(client: reqwest::Client, url: reqwest::Url) -> Option<IceServer> {
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    match resp.json::<TurnCredentialsResponse>().await {
        Ok(creds) if !creds.urls.is_empty() => Some(IceServer {
            urls: creds.urls,
            username: Some(creds.username),
            credential: Some(creds.credential),
        }),
        _ => None,
    }
}

pub async fn fetch_turn_credentials(relay_server_url: &str, peer_id: &str) -> Option<IceServer> {
    let url = turn_credentials_url(relay_server_url, peer_id)?;

    if let Ok(client) = reqwest::Client::builder().timeout(FETCH_TIMEOUT).build() {
        if let Some(server) = fetch_with_client(client, url.clone()).await {
            return Some(server);
        }
    }

    if !fallback_applies(relay_server_url) {
        return None;
    }
    let ip: IpAddr = RELAY_FALLBACK_IP.parse().ok()?;
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .resolve(RELAY_HOST, SocketAddr::new(ip, 443))
        .build()
        .ok()?;
    fetch_with_client(client, url).await
}

pub fn fetch_turn_credentials_blocking(relay_server_url: &str, peer_id: &str) -> Option<IceServer> {
    let relay_server_url = relay_server_url.to_string();
    let peer_id = peer_id.to_string();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        runtime.block_on(fetch_turn_credentials(&relay_server_url, &peer_id))
    })
    .join()
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_https_endpoint_from_wss_url() {
        let url = turn_credentials_url("wss://relay.plenumonline.me/ws", "peer-1").unwrap();
        assert_eq!(
            url.as_str(),
            "https://relay.plenumonline.me/turn-credentials?peer_id=peer-1"
        );
    }

    #[test]
    fn derives_http_endpoint_from_ws_url() {
        let url = turn_credentials_url("ws://localhost:8080/ws", "p").unwrap();
        assert_eq!(
            url.as_str(),
            "http://localhost:8080/turn-credentials?peer_id=p"
        );
    }

    #[test]
    fn empty_relay_url_yields_none() {
        assert!(turn_credentials_url("", "p").is_none());
        assert!(turn_credentials_url("   ", "p").is_none());
    }
}
