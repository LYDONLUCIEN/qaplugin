use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub device_id: String,
    pub title: String,
    pub prompt: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub turn_count: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TurnRecord {
    pub id: String,
    pub session_id: String,
    pub prompt: String,
    pub screenshot_b64: String,
    pub screenshot_mime: String,
    pub answer: String,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum QaEvent {
    Capturing,
    Uploading,
    Screenshot {
        image_b64: String,
        mime_type: String,
    },
    Streaming {
        delta: String,
    },
    Done {
        answer: String,
    },
    Error {
        message: String,
    },
    DeviceStatus {
        connected: bool,
    },
    Snapshot {
        connected: bool,
        screenshot_b64: Option<String>,
        screenshot_mime: Option<String>,
        answer: String,
        status: String,
    },
    SessionList {
        sessions: Vec<SessionSummary>,
        active_session_id: Option<String>,
    },
    SessionDetail {
        session: SessionSummary,
        turns: Vec<TurnRecord>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnswerRequest {
    pub device_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    pub question: Option<String>,
    /// Cloud-side model profile selected by this desktop device. API keys never
    /// leave the cloud; this only names a profile configured in `.env.cloud`.
    #[serde(default)]
    pub model_profile: Option<String>,
    pub image_b64: String,
    pub mime_type: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DeviceCommand {
    Trigger {
        question: Option<String>,
        #[serde(default)]
        session_id: Option<String>,
    },
    Ping,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WebClientMessage {
    Trigger {
        question: Option<String>,
        #[serde(default)]
        session_id: Option<String>,
    },
    CreateSession {
        title: Option<String>,
        prompt: Option<String>,
    },
    SelectSession {
        session_id: String,
    },
    UpdateSession {
        session_id: String,
        title: Option<String>,
        prompt: Option<String>,
    },
    Ping,
}
