//! Lean outbound notification payloads and ESP-IDF HTTPS delivery.
//!
//! Payload construction is host-pure and re-included by `dcentaxe-core`.
//! Delivery is outbound-only, best-effort, and adds no HTTP URI handlers.

use crate::config::NotificationsConfig;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    ShareMilestone,
    Thermal,
    Failover,
    Ota,
}

#[derive(Debug, Clone)]
pub struct NotificationEvent<'a> {
    pub kind: NotificationKind,
    pub title: &'a str,
    pub message: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookRequest {
    pub service: &'static str,
    pub url: String,
    pub json_body: String,
}

pub fn build_webhook_requests(
    config: &NotificationsConfig,
    event: &NotificationEvent<'_>,
) -> Vec<WebhookRequest> {
    if !config.enabled || !event_enabled(config, event.kind) {
        return Vec::new();
    }
    let text = format!("DCENT_axe — {}\n{}", event.title, event.message);
    let mut requests = Vec::new();
    if !config.telegram_bot_token.is_empty() && !config.telegram_chat_id.is_empty() {
        requests.push(WebhookRequest {
            service: "telegram",
            url: format!(
                "https://api.telegram.org/bot{}/sendMessage",
                config.telegram_bot_token
            ),
            json_body: serde_json::json!({
                "chat_id": config.telegram_chat_id,
                "text": text,
                "disable_web_page_preview": true,
            })
            .to_string(),
        });
    }
    if !config.discord_webhook_url.is_empty() {
        requests.push(WebhookRequest {
            service: "discord",
            url: config.discord_webhook_url.clone(),
            json_body: serde_json::json!({"content": text}).to_string(),
        });
    }
    if !config.slack_webhook_url.is_empty() {
        requests.push(WebhookRequest {
            service: "slack",
            url: config.slack_webhook_url.clone(),
            json_body: serde_json::json!({"text": text}).to_string(),
        });
    }
    requests
}

fn event_enabled(config: &NotificationsConfig, kind: NotificationKind) -> bool {
    match kind {
        NotificationKind::ShareMilestone => config.share_milestone > 0,
        NotificationKind::Thermal => config.thermal_alerts,
        NotificationKind::Failover => config.failover_alerts,
        NotificationKind::Ota => config.ota_alerts,
    }
}

#[cfg(target_os = "espidf")]
pub fn send_event(config: &NotificationsConfig, event: NotificationEvent<'_>) {
    use embedded_svc::http::client::Client as HttpClient;
    use embedded_svc::io::Write;
    use esp_idf_svc::http::client::{Configuration, EspHttpConnection};

    for webhook in build_webhook_requests(config, &event) {
        let length = webhook.json_body.len().to_string();
        let http_config = Configuration {
            timeout: Some(core::time::Duration::from_secs(8)),
            crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
            ..Default::default()
        };
        let result = (|| -> Result<u16, String> {
            let connection = EspHttpConnection::new(&http_config).map_err(|e| e.to_string())?;
            let mut client = HttpClient::wrap(connection);
            let headers = [
                ("content-type", "application/json"),
                ("content-length", length.as_str()),
            ];
            let mut request = client
                .post(&webhook.url, &headers)
                .map_err(|e| e.to_string())?;
            request
                .write_all(webhook.json_body.as_bytes())
                .map_err(|e| e.to_string())?;
            request.flush().map_err(|e| e.to_string())?;
            let response = request.submit().map_err(|e| e.to_string())?;
            Ok(response.status())
        })();
        match result {
            Ok(status) if (200..300).contains(&status) => {
                log::info!("Notification delivered via {}", webhook.service)
            }
            Ok(status) => log::warn!("Notification {} returned HTTP {}", webhook.service, status),
            Err(error) => log::warn!(
                "Notification {} delivery failed: {}",
                webhook.service,
                error
            ),
        }
    }
}

#[cfg(target_os = "espidf")]
pub fn spawn_event(
    config: NotificationsConfig,
    kind: NotificationKind,
    title: impl Into<String>,
    message: impl Into<String>,
) {
    if !config.enabled {
        return;
    }
    let title = title.into();
    let message = message.into();
    let _ = std::thread::Builder::new()
        .name("notify".to_string())
        .stack_size(8 * 1024)
        .spawn(move || {
            send_event(
                &config,
                NotificationEvent {
                    kind,
                    title: &title,
                    message: &message,
                },
            )
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured() -> NotificationsConfig {
        NotificationsConfig {
            enabled: true,
            telegram_bot_token: "telegram-secret".into(),
            telegram_chat_id: "1234".into(),
            discord_webhook_url: "https://discord.example/discord-secret".into(),
            slack_webhook_url: "https://slack.example/slack-secret".into(),
            share_milestone: 100,
            ..NotificationsConfig::default()
        }
    }

    #[test]
    fn default_off_builds_no_outbound_requests() {
        let requests = build_webhook_requests(
            &NotificationsConfig::default(),
            &NotificationEvent {
                kind: NotificationKind::Thermal,
                title: "Thermal warning",
                message: "80 C",
            },
        );
        assert!(requests.is_empty());
    }

    #[test]
    fn all_payloads_are_valid_json_and_never_embed_transport_secrets() {
        let requests = build_webhook_requests(
            &configured(),
            &NotificationEvent {
                kind: NotificationKind::Failover,
                title: "Pool failover",
                message: "Fallback active",
            },
        );
        assert_eq!(requests.len(), 3);
        for request in requests {
            serde_json::from_str::<serde_json::Value>(&request.json_body).expect("valid JSON");
            assert!(!request.json_body.contains("telegram-secret"));
            assert!(!request.json_body.contains("discord-secret"));
            assert!(!request.json_body.contains("slack-secret"));
        }
    }

    #[test]
    fn event_toggles_gate_individual_payloads() {
        let mut config = configured();
        config.failover_alerts = false;
        assert!(build_webhook_requests(
            &config,
            &NotificationEvent {
                kind: NotificationKind::Failover,
                title: "Failover",
                message: "ignored",
            },
        )
        .is_empty());
    }
}
