//! Ecosystem mailer — SendGrid-backed transactional email for
//! datachain.network, dcscan.io, and Datachain Rope services.
//!
//! Configuration comes from the environment (see `.env` / the systemd
//! `EnvironmentFile` on the production nodes):
//!
//! | Variable             | Purpose                                        |
//! |----------------------|------------------------------------------------|
//! | `EMAIL_PASS`         | SendGrid API key (the SMTP password *is* the key) |
//! | `EMAIL_FROM`         | Display From, e.g. `Datachain Network <noreply@datachain.one>` |
//! | `EMAIL_REPLY_TO`     | Default Reply-To (support@datachain.one)       |
//! | `DEFAULT_FROM_EMAIL` | Fallback from address                          |
//! | `DEFAULT_FROM_NAME`  | Fallback from display name                     |
//! | `CONTACT_RECIPIENT`  | Where contact-form submissions are delivered (default `contact@datachain.one`) |
//!
//! Mail is sent through the SendGrid v3 HTTP API (`POST
//! https://api.sendgrid.com/v3/mail/send`) rather than SMTP so no extra
//! crate or open port is needed. When `EMAIL_PASS` is not configured the
//! mailer reports itself as disabled and the contact endpoint returns 503
//! instead of silently dropping mail.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::RwLock;

const SENDGRID_SEND_URL: &str = "https://api.sendgrid.com/v3/mail/send";

/// Contact-form abuse guard: max submissions per IP per fixed window.
const CONTACT_MAX_PER_WINDOW: u32 = 5;
const CONTACT_WINDOW_SECS: i64 = 3600;

#[derive(Clone)]
pub struct Mailer {
    api_key: Option<String>,
    from_email: String,
    from_name: String,
    reply_to: String,
    contact_recipient: String,
    http: reqwest::Client,
    /// ip -> (window_start_unix, count) for the public contact endpoint.
    contact_hits: Arc<RwLock<HashMap<String, (i64, u32)>>>,
}

/// Parse `Display Name <addr@host>` into (name, email).
fn parse_mailbox(raw: &str, fallback_name: &str) -> (String, String) {
    let raw = raw.trim();
    if let (Some(lt), Some(gt)) = (raw.find('<'), raw.rfind('>')) {
        if gt > lt {
            let name = raw[..lt].trim().trim_matches('"').to_string();
            let email = raw[lt + 1..gt].trim().to_string();
            let name = if name.is_empty() {
                fallback_name.to_string()
            } else {
                name
            };
            return (name, email);
        }
    }
    (fallback_name.to_string(), raw.to_string())
}

impl Mailer {
    pub fn from_env() -> Self {
        let api_key = std::env::var("EMAIL_PASS")
            .ok()
            .or_else(|| std::env::var("SENDGRID_API_KEY").ok())
            .filter(|k| k.starts_with("SG."));

        let default_from_email = std::env::var("DEFAULT_FROM_EMAIL")
            .unwrap_or_else(|_| "noreply@datachain.one".to_string());
        let default_from_name = std::env::var("DEFAULT_FROM_NAME")
            .unwrap_or_else(|_| "Datachain Foundation".to_string());

        let (from_name, from_email) = match std::env::var("EMAIL_FROM") {
            Ok(raw) => parse_mailbox(&raw, &default_from_name),
            Err(_) => (default_from_name.clone(), default_from_email.clone()),
        };

        let reply_to = std::env::var("EMAIL_REPLY_TO")
            .map(|raw| parse_mailbox(&raw, "").1)
            .unwrap_or_else(|_| "support@datachain.one".to_string());

        let contact_recipient = std::env::var("CONTACT_RECIPIENT")
            .unwrap_or_else(|_| "contact@datachain.one".to_string());

        if api_key.is_none() {
            tracing::warn!(
                "mailer: EMAIL_PASS / SENDGRID_API_KEY not configured — outbound email disabled"
            );
        } else {
            tracing::info!(
                "mailer: SendGrid configured (from {} <{}>, contact -> {})",
                from_name,
                from_email,
                contact_recipient
            );
        }

        Self {
            api_key,
            from_email,
            from_name,
            reply_to,
            contact_recipient,
            http: reqwest::Client::new(),
            contact_hits: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn enabled(&self) -> bool {
        self.api_key.is_some()
    }

    /// Send a plain-text email. `reply_to` overrides the default Reply-To
    /// (used by the contact form so replies go back to the submitter).
    pub async fn send(
        &self,
        to_email: &str,
        subject: &str,
        text_body: &str,
        reply_to: Option<&str>,
    ) -> anyhow::Result<()> {
        let api_key = self
            .api_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("mailer disabled: no SendGrid API key configured"))?;

        let payload = json!({
            "personalizations": [{ "to": [{ "email": to_email }] }],
            "from": { "email": self.from_email, "name": self.from_name },
            "reply_to": { "email": reply_to.unwrap_or(&self.reply_to) },
            "subject": subject,
            "content": [{ "type": "text/plain", "value": text_body }]
        });

        let resp = self
            .http
            .post(SENDGRID_SEND_URL)
            .bearer_auth(api_key)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("SendGrid rejected mail ({}): {}", status, body);
        }
        Ok(())
    }

    /// Fire-and-forget variant for notifications that must never block or
    /// fail the caller's request (e.g. API-key-created notices).
    pub fn send_background(&self, to_email: String, subject: String, text_body: String) {
        if !self.enabled() {
            return;
        }
        let mailer = self.clone();
        tokio::spawn(async move {
            if let Err(e) = mailer.send(&to_email, &subject, &text_body, None).await {
                tracing::warn!("mailer: background send to {} failed: {}", to_email, e);
            }
        });
    }

    /// Fixed-window per-IP rate limit for the public contact endpoint.
    async fn contact_allowed(&self, ip: &str) -> bool {
        let now = chrono::Utc::now().timestamp();
        let mut hits = self.contact_hits.write().await;
        hits.retain(|_, (start, _)| now - *start < CONTACT_WINDOW_SECS);
        let entry = hits.entry(ip.to_string()).or_insert((now, 0));
        if now - entry.0 >= CONTACT_WINDOW_SECS {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= CONTACT_MAX_PER_WINDOW
    }
}

#[derive(Deserialize)]
pub struct ContactForm {
    pub name: String,
    pub email: String,
    #[serde(default)]
    pub subject: String,
    pub message: String,
    /// Which site the submission came from (datachain.network, dcscan.io…).
    #[serde(default)]
    pub source: String,
}

fn bad_request(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "success": false, "error": msg })),
    )
        .into_response()
}

/// POST /api/v1/contact — public contact-form relay.
pub async fn contact(
    State(state): State<Arc<crate::AppState>>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(form): Json<ContactForm>,
) -> Response {
    let mailer = &state.mailer;
    if !mailer.enabled() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "success": false,
                "error": "Email service not configured. Please email contact@datachain.one directly."
            })),
        )
            .into_response();
    }

    let name = form.name.trim();
    let email = form.email.trim();
    let message = form.message.trim();
    if name.is_empty() || name.len() > 200 {
        return bad_request("name is required (max 200 chars)");
    }
    if !email.contains('@') || email.len() > 320 {
        return bad_request("a valid email address is required");
    }
    if message.is_empty() || message.len() > 10_000 {
        return bad_request("message is required (max 10000 chars)");
    }

    // CERBER WATCH — `name`/`subject`/`source` are structured fields; the
    // body of the outbound email includes them verbatim. `message` is
    // classified as free text (CHAT_FIELDS) so ordinary prose is never
    // false-positived, but a definite-attack pattern (SQL comment
    // injection, `UNION SELECT`, script tags) is still rejected regardless
    // of field.
    if let Err(resp) = crate::security_guard::validate_fields(&[
        ("name", name),
        ("subject", form.subject.trim()),
        ("source", form.source.trim()),
        ("message", message),
    ]) {
        return resp.into_response();
    }

    // Client IP: honour X-Forwarded-For from nginx, fall back to the peer.
    let ip = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| peer.ip().to_string());

    if !mailer.contact_allowed(&ip).await {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "success": false,
                "error": "Too many messages from this address — please try again later."
            })),
        )
            .into_response();
    }

    let source = if form.source.trim().is_empty() {
        "datachain.network".to_string()
    } else {
        form.source.trim().chars().take(100).collect()
    };
    let subject_line = if form.subject.trim().is_empty() {
        format!("[{}] Contact form message from {}", source, name)
    } else {
        format!(
            "[{}] {}",
            source,
            form.subject.trim().chars().take(200).collect::<String>()
        )
    };
    let body = format!(
        "New contact form submission\n\
         ---------------------------\n\
         Site:    {}\n\
         Name:    {}\n\
         Email:   {}\n\
         IP:      {}\n\
         Time:    {}\n\
         ---------------------------\n\n\
         {}\n",
        source,
        name,
        email,
        ip,
        chrono::Utc::now().to_rfc3339(),
        message
    );

    match mailer
        .send(&mailer.contact_recipient.clone(), &subject_line, &body, Some(email))
        .await
    {
        Ok(()) => Json(json!({
            "success": true,
            "message": "Your message has been sent. We will get back to you shortly."
        }))
        .into_response(),
        Err(e) => {
            tracing::error!("mailer: contact form delivery failed: {}", e);
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "success": false,
                    "error": "Delivery failed — please email contact@datachain.one directly."
                })),
            )
                .into_response()
        }
    }
}
