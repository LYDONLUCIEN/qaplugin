use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};

#[derive(Clone, Debug)]
pub struct CloudConfig {
    pub bind_addr: SocketAddr,
    pub web_dir: PathBuf,
    pub device_tokens: HashMap<String, String>,
    pub max_image_bytes: usize,
    pub db_path: PathBuf,
    pub admin_username: String,
    pub bootstrap_admin_password: Option<String>,
    pub auth_session_seconds: i64,
    pub cookie_secure: bool,
}

impl CloudConfig {
    pub fn from_env() -> Result<Self> {
        let bind_addr: SocketAddr = std::env::var("QA_BIND_ADDR")
            .unwrap_or_else(|_| "0.0.0.0:6060".to_string())
            .parse()
            .context("invalid QA_BIND_ADDR")?;
        let web_dir = std::env::var("QA_WEB_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("apps/cloud-web/dist"));

        let mut device_tokens = HashMap::new();
        if let Ok(raw) = std::env::var("QA_DEVICE_TOKENS") {
            for pair in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                let (device_id, token) = pair
                    .split_once('=')
                    .ok_or_else(|| anyhow!("QA_DEVICE_TOKENS entries must use device-id=token"))?;
                validate_device_id(device_id)?;
                if token.trim().is_empty() {
                    return Err(anyhow!("empty token for device '{device_id}'"));
                }
                device_tokens.insert(device_id.trim().to_string(), token.trim().to_string());
            }
        }

        if device_tokens.is_empty() {
            let device_id = required("QA_DEVICE_ID")?;
            validate_device_id(&device_id)?;
            device_tokens.insert(device_id, required("QA_DEVICE_TOKEN")?);
        }

        let max_image_mb = std::env::var("QA_MAX_IMAGE_MB")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(20);
        let db_path = std::env::var("QA_DB_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data/qa-snapshot.db"));
        let admin_username =
            std::env::var("QA_ADMIN_USERNAME").unwrap_or_else(|_| "admin".to_string());
        let bootstrap_admin_password = std::env::var("QA_ADMIN_PASSWORD")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                std::env::var("QA_WEB_TOKEN")
                    .ok()
                    .filter(|value| !value.is_empty())
            });
        let auth_session_hours = std::env::var("QA_AUTH_SESSION_HOURS")
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|hours| (1..=24 * 90).contains(hours))
            .unwrap_or(24 * 7);
        let cookie_secure = std::env::var("QA_COOKIE_SECURE")
            .ok()
            .and_then(|value| parse_bool(&value))
            .unwrap_or_else(|| !bind_addr.ip().is_loopback());

        Ok(Self {
            bind_addr,
            web_dir,
            device_tokens,
            max_image_bytes: max_image_mb * 1024 * 1024,
            db_path,
            admin_username,
            bootstrap_admin_password,
            auth_session_seconds: auth_session_hours * 60 * 60,
            cookie_secure,
        })
    }

    pub fn device_token(&self, device_id: &str) -> Option<&str> {
        self.device_tokens.get(device_id).map(String::as_str)
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

pub fn validate_device_id(device_id: &str) -> Result<()> {
    if device_id.is_empty()
        || device_id.len() > 64
        || !device_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(anyhow!(
            "device id must be 1-64 characters using letters, numbers, '-' or '_'"
        ));
    }
    Ok(())
}

fn required(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| anyhow!("{name} is required"))
}
