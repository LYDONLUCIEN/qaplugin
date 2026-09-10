use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use futures_util::{SinkExt, Stream, StreamExt};
use qa_protocol::{AnswerRequest, DeviceCommand, QaEvent};
use reqwest::header::{HeaderValue, AUTHORIZATION};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

static CONNECTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudConfig {
    pub cloud_url: String,
    pub web_url: String,
    pub device_id: String,
    pub device_token: String,
    #[serde(default)]
    pub model_profile: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ModelProfile {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ModelProfilesResponse {
    pub default_model_profile: String,
    pub profiles: Vec<ModelProfile>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModelProfileTestResponse {
    #[serde(rename = "profileId")]
    pub profile_id: String,
    pub sample: String,
}

impl CloudConfig {
    pub fn from_env() -> Result<Self> {
        let cloud_url = required("QA_CLOUD_URL")?.trim_end_matches('/').to_string();
        if !cloud_url.starts_with("http://") && !cloud_url.starts_with("https://") {
            return Err(anyhow!("QA_CLOUD_URL must start with http:// or https://"));
        }
        let web_url = std::env::var("QA_WEB_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| cloud_url.clone())
            .trim_end_matches('/')
            .to_string();
        let device_id = required("QA_DEVICE_ID")?;
        if device_id.is_empty()
            || device_id.len() > 64
            || !device_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(anyhow!("invalid QA_DEVICE_ID"));
        }
        Self::with_model_profile(
            cloud_url,
            web_url,
            device_id,
            required("QA_DEVICE_TOKEN")?,
            std::env::var("QA_MODEL_PROFILE").unwrap_or_default(),
        )
    }

    pub fn new(
        cloud_url: String,
        web_url: String,
        device_id: String,
        device_token: String,
    ) -> Result<Self> {
        Self::with_model_profile(cloud_url, web_url, device_id, device_token, String::new())
    }

    pub fn with_model_profile(
        cloud_url: String,
        web_url: String,
        device_id: String,
        device_token: String,
        model_profile: String,
    ) -> Result<Self> {
        let cloud_url = cloud_url.trim().trim_end_matches('/').to_string();
        let web_url = web_url.trim().trim_end_matches('/').to_string();
        let device_id = device_id.trim().to_string();
        let device_token = device_token.trim().to_string();
        if !cloud_url.starts_with("http://") && !cloud_url.starts_with("https://") {
            return Err(anyhow!("cloud URL must start with http:// or https://"));
        }
        if !web_url.starts_with("http://") && !web_url.starts_with("https://") {
            return Err(anyhow!("web URL must start with http:// or https://"));
        }
        if device_token.is_empty() {
            return Err(anyhow!("device token cannot be empty"));
        }
        if device_id.is_empty()
            || device_id.len() > 64
            || !device_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(anyhow!("invalid device ID"));
        }
        Ok(Self {
            cloud_url,
            web_url,
            device_id,
            device_token,
            model_profile: model_profile.trim().to_string(),
        })
    }

    pub fn phone_url(&self) -> String {
        format!("{}/?device_id={}", self.web_url, self.device_id)
    }

    fn answer_url(&self) -> String {
        format!("{}/v1/answers/stream", self.cloud_url)
    }

    fn command_url(&self) -> String {
        let base = if let Some(rest) = self.cloud_url.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = self.cloud_url.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            self.cloud_url.clone()
        };
        format!("{base}/v1/devices/connect?device_id={}", self.device_id)
    }
}

#[derive(Clone)]
pub struct CloudConfigState {
    sender: watch::Sender<Option<CloudConfig>>,
    path: PathBuf,
}

pub fn init(app: &AppHandle) {
    let path = config_path(app).unwrap_or_else(|_| PathBuf::from("qa-cloud-config.json"));
    let saved = load_file(&path).ok();
    let config = CloudConfig::from_env()
        .ok()
        .map(|mut from_env| {
            // Development launch scripts often provide the connection variables
            // from `.env.desktop`. Preserve a profile chosen in the Control UI
            // unless QA_MODEL_PROFILE explicitly overrides it for this device.
            if from_env.model_profile.is_empty() {
                if let Some(saved) = saved.as_ref().filter(|saved| {
                    saved.cloud_url == from_env.cloud_url && saved.device_id == from_env.device_id
                }) {
                    from_env.model_profile = saved.model_profile.clone();
                }
            }
            from_env
        })
        .or(saved);
    let (sender, _) = watch::channel(config);
    app.manage(CloudConfigState { sender, path });
}

pub fn current(app: &AppHandle) -> Option<CloudConfig> {
    app.try_state::<CloudConfigState>()
        .and_then(|state| state.sender.borrow().clone())
}

pub async fn save(
    app: &AppHandle,
    cloud_url: String,
    web_url: String,
    device_id: String,
    device_token: Option<String>,
    model_profile: Option<String>,
) -> Result<CloudConfig> {
    let state = app
        .try_state::<CloudConfigState>()
        .ok_or_else(|| anyhow!("cloud config state is not initialized"))?;
    let token = device_token
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            state
                .sender
                .borrow()
                .as_ref()
                .map(|config| config.device_token.clone())
        })
        .ok_or_else(|| anyhow!("device token is required"))?;
    let model_profile = model_profile
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            state
                .sender
                .borrow()
                .as_ref()
                .map(|config| config.model_profile.clone())
        })
        .unwrap_or_default();
    let config =
        CloudConfig::with_model_profile(cloud_url, web_url, device_id, token, model_profile)?;
    if let Some(parent) = state.path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let body = serde_json::to_vec_pretty(&config)?;
    tokio::fs::write(&state.path, body).await?;
    state.sender.send_replace(Some(config.clone()));
    Ok(config)
}

fn config_path(app: &AppHandle) -> Result<PathBuf> {
    Ok(app.path().app_config_dir()?.join("cloud.json"))
}

fn load_file(path: &PathBuf) -> Result<CloudConfig> {
    let body = std::fs::read(path)?;
    let saved: CloudConfig = serde_json::from_slice(&body)?;
    CloudConfig::with_model_profile(
        saved.cloud_url,
        saved.web_url,
        saved.device_id,
        saved.device_token,
        saved.model_profile,
    )
}

pub fn connected() -> bool {
    CONNECTED.load(Ordering::Relaxed)
}

pub async fn test_connection(
    cloud_url: String,
    device_id: String,
    device_token: String,
) -> Result<ModelProfilesResponse> {
    let config = CloudConfig::new(cloud_url.clone(), cloud_url, device_id, device_token)?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .user_agent("qa-snapshot-desktop/0.1")
        .build()?;
    let health = client
        .get(format!("{}/healthz", config.cloud_url))
        .send()
        .await
        .context("cloud health check failed")?;
    if !health.status().is_success() {
        return Err(anyhow!("cloud health check returned {}", health.status()));
    }
    let profiles = client
        .get(format!(
            "{}/v1/model-profiles?device_id={}",
            config.cloud_url, config.device_id
        ))
        .bearer_auth(&config.device_token)
        .send()
        .await
        .context("model profile check failed")?;
    if !profiles.status().is_success() {
        let status = profiles.status();
        let body = profiles.text().await.unwrap_or_default();
        return Err(anyhow!("model profile check returned {status}: {body}"));
    }
    let payload = profiles
        .json::<ModelProfilesResponse>()
        .await
        .context("invalid model profile response")?;
    if payload.profiles.is_empty() {
        return Err(anyhow!("cloud returned no model profiles"));
    }
    Ok(payload)
}

pub async fn test_model_profile(
    cloud_url: String,
    device_id: String,
    device_token: String,
    profile_id: String,
) -> Result<ModelProfileTestResponse> {
    let config = CloudConfig::new(cloud_url.clone(), cloud_url, device_id, device_token)?;
    if profile_id.trim().is_empty()
        || !profile_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(anyhow!("invalid model profile"));
    }
    let response = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(35))
        .user_agent("qa-snapshot-desktop/0.1")
        .build()?
        .post(format!(
            "{}/v1/model-profiles/{}/test?device_id={}",
            config.cloud_url, profile_id, config.device_id
        ))
        .bearer_auth(&config.device_token)
        .send()
        .await
        .context("model test request failed")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("model test returned {status}: {body}"));
    }
    response
        .json::<ModelProfileTestResponse>()
        .await
        .context("invalid model test response")
}

type EventStream = Pin<Box<dyn Stream<Item = Result<QaEvent>> + Send>>;

pub async fn stream_answer(
    config: &CloudConfig,
    image: &[u8],
    image_mime: &str,
    question: Option<String>,
    session_id: Option<String>,
) -> Result<EventStream> {
    let request = AnswerRequest {
        device_id: config.device_id.clone(),
        session_id,
        question,
        model_profile: (!config.model_profile.is_empty()).then(|| config.model_profile.clone()),
        image_b64: base64::engine::general_purpose::STANDARD.encode(image),
        mime_type: image_mime.to_string(),
    };
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .user_agent("qa-snapshot-desktop/0.1")
        .build()?;
    let response = client
        .post(config.answer_url())
        .bearer_auth(&config.device_token)
        .json(&request)
        .send()
        .await
        .context("cloud request failed")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("cloud returned {status}: {body}"));
    }
    Ok(Box::pin(parse_sse(response.bytes_stream())))
}

fn parse_sse<S>(bytes: S) -> impl Stream<Item = Result<QaEvent>> + Send
where
    S: Stream<Item = reqwest::Result<bytes::Bytes>> + Unpin + Send + 'static,
{
    async_stream::try_stream! {
        let mut bytes = bytes;
        let mut buffer = Vec::new();
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.context("cloud stream read failed")?;
            buffer.extend_from_slice(&chunk);
            while let Some((index, separator_len)) = sse_boundary(&buffer) {
                let block = String::from_utf8_lossy(&buffer[..index]).into_owned();
                buffer.drain(..index + separator_len);
                for line in block.lines() {
                    let Some(data) = line.strip_prefix("data:") else {
                        continue;
                    };
                    let data = data.trim();
                    if data.is_empty() {
                        continue;
                    }
                    let event = serde_json::from_str::<QaEvent>(data)
                        .context("invalid cloud event")?;
                    yield event;
                }
            }
        }
    }
}

fn sse_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n");
    let crlf = buffer.windows(4).position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(left), Some(right)) if left <= right => Some((left, 2)),
        (Some(_), Some(right)) => Some((right, 4)),
        (Some(index), None) => Some((index, 2)),
        (None, Some(index)) => Some((index, 4)),
        (None, None) => None,
    }
}

pub fn start_command_loop(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<CloudConfigState>() else {
            log::error!("cloud configuration state is unavailable");
            return;
        };
        let mut configs = state.sender.subscribe();

        loop {
            let Some(config) = configs.borrow().clone() else {
                CONNECTED.store(false, Ordering::Relaxed);
                if configs.changed().await.is_err() {
                    return;
                }
                continue;
            };

            let connection = run_command_connection(app.clone(), &config);
            tokio::pin!(connection);
            tokio::select! {
                result = &mut connection => {
                    if let Err(error) = result {
                        log::warn!("cloud command connection closed: {error}");
                    }
                }
                changed = configs.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    log::info!("cloud configuration changed; reconnecting");
                }
            }
            CONNECTED.store(false, Ordering::Relaxed);
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                changed = configs.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
        }
    });
}

async fn run_command_connection(app: AppHandle, config: &CloudConfig) -> Result<()> {
    let mut request = config
        .command_url()
        .into_client_request()
        .context("invalid cloud websocket URL")?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", config.device_token))?,
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .context("cloud websocket connection failed")?;
    CONNECTED.store(true, Ordering::Relaxed);
    log::info!("connected to cloud as device '{}'", config.device_id);

    let mut heartbeat = tokio::time::interval(Duration::from_secs(25));
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                socket.send(Message::Ping(Vec::new())).await?;
            }
            incoming = socket.next() => {
                let Some(message) = incoming else {
                    return Err(anyhow!("cloud websocket ended"));
                };
                match message? {
                    Message::Text(text) => {
                        match serde_json::from_str::<DeviceCommand>(&text) {
                            Ok(DeviceCommand::Trigger { question, session_id }) => {
                                if let Err(message) = crate::screenshot::capture_and_ask(app.clone(), question, session_id).await {
                                    let report = QaEvent::Error { message };
                                    socket.send(Message::Text(serde_json::to_string(&report)?)).await?;
                                }
                            }
                            Ok(DeviceCommand::Ping) => {
                                socket.send(Message::Pong(Vec::new())).await?;
                            }
                            Err(error) => log::warn!("invalid cloud command: {error}"),
                        }
                    }
                    Message::Ping(payload) => socket.send(Message::Pong(payload)).await?,
                    Message::Close(_) => return Ok(()),
                    _ => {}
                }
            }
        }
    }
}

fn required(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("{name} is required"))
}

#[cfg(test)]
mod tests {
    use super::parse_sse;
    use bytes::Bytes;
    use futures_util::{stream, StreamExt};
    use qa_protocol::QaEvent;

    #[tokio::test]
    async fn parses_streaming_events_split_inside_chinese_text() {
        let body = concat!(
            "data: {\"type\":\"Streaming\",\"delta\":\"你\"}\r\n\r\n",
            "data: {\"type\":\"Streaming\",\"delta\":\"好\"}\n\n",
        );
        let first_split = body.find('你').unwrap() + 1;
        let second_split = body.find('好').unwrap() + 2;
        let chunks: Vec<reqwest::Result<Bytes>> = vec![
            Ok(Bytes::copy_from_slice(&body.as_bytes()[..first_split])),
            Ok(Bytes::copy_from_slice(
                &body.as_bytes()[first_split..second_split],
            )),
            Ok(Bytes::copy_from_slice(&body.as_bytes()[second_split..])),
        ];

        let events = parse_sse(stream::iter(chunks));
        futures_util::pin_mut!(events);
        let mut collected = Vec::new();
        while let Some(event) = events.next().await {
            match event.unwrap() {
                QaEvent::Streaming { delta } => collected.push(delta),
                other => panic!("unexpected event: {other:?}"),
            }
        }

        assert_eq!(collected, ["你", "好"]);
    }
}
