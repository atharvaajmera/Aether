//! DNS-sinkhole fallback addressing for the official Plenum relay.
use crate::signaling::IceServer;

pub const RELAY_HOST: &str = "relay.plenumonline.me";

pub const RELAY_FALLBACK_IP: &str = "13.232.226.146";

pub fn fallback_applies(url: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(|host| host == RELAY_HOST))
        .unwrap_or(false)
}

pub fn rewrite_host_to_fallback_ip(url: &str) -> String {
    url.replace(RELAY_HOST, RELAY_FALLBACK_IP)
}

pub fn rewrite_ice_servers_to_fallback_ip(servers: &[IceServer]) -> Vec<IceServer> {
    servers
        .iter()
        .map(|server| IceServer {
            urls: server
                .urls
                .iter()
                .map(|u| rewrite_host_to_fallback_ip(u))
                .collect(),
            username: server.username.clone(),
            credential: server.credential.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_applies_only_to_official_relay() {
        assert!(fallback_applies("wss://relay.plenumonline.me/ws"));
        assert!(fallback_applies("https://relay.plenumonline.me/turn-credentials"));
        assert!(!fallback_applies("wss://example.com/ws"));
        assert!(!fallback_applies("not a url"));
        assert!(!fallback_applies(""));
    }

    #[test]
    fn rewrites_stun_and_turn_urls() {
        assert_eq!(
            rewrite_host_to_fallback_ip("stun:relay.plenumonline.me:3478"),
            format!("stun:{RELAY_FALLBACK_IP}:3478")
        );
        assert_eq!(
            rewrite_host_to_fallback_ip("turn:relay.plenumonline.me:3478?transport=udp"),
            format!("turn:{RELAY_FALLBACK_IP}:3478?transport=udp")
        );
        assert_eq!(
            rewrite_host_to_fallback_ip("stun:stun.l.google.com:19302"),
            "stun:stun.l.google.com:19302"
        );
    }

    #[test]
    fn rewrites_ice_server_list_preserving_credentials() {
        let servers = vec![
            IceServer {
                urls: vec!["stun:relay.plenumonline.me:3478".into()],
                username: None,
                credential: None,
            },
            IceServer {
                urls: vec!["turn:relay.plenumonline.me:3478?transport=udp".into()],
                username: Some("user".into()),
                credential: Some("pass".into()),
            },
        ];
        let rewritten = rewrite_ice_servers_to_fallback_ip(&servers);
        assert_eq!(rewritten[0].urls[0], format!("stun:{RELAY_FALLBACK_IP}:3478"));
        assert_eq!(
            rewritten[1].urls[0],
            format!("turn:{RELAY_FALLBACK_IP}:3478?transport=udp")
        );
        assert_eq!(rewritten[1].username.as_deref(), Some("user"));
        assert_eq!(rewritten[1].credential.as_deref(), Some("pass"));
    }
}
