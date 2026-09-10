mod ai;
mod auth;
mod config;
mod store;

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use futures_util::{SinkExt, StreamExt};
use qa_protocol::{AnswerRequest, DeviceCommand, QaEvent, WebClientMessage};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::config::CloudConfig;
use crate::store::{AdminUserSummary, AuthUser, DeviceSummary, Store};

#[derive(Clone, Default)]
struct LatestSnapshot {
    screenshot_b64: Option<String>,
    screenshot_mime: Option<String>,
    answer: String,
    status: String,
}

struct DeviceRuntime {
    events: broadcast::Sender<QaEvent>,
    commands: broadcast::Sender<DeviceCommand>,
    latest: RwLock<LatestSnapshot>,
    connections: AtomicUsize,
    answer_lock: Mutex<()>,
}

impl DeviceRuntime {
    fn new() -> Self {
        let (events, _) = broadcast::channel(512);
        let (commands, _) = broadcast::channel(64);
        Self {
            events,
            commands,
            latest: RwLock::new(LatestSnapshot {
                status: "idle".to_string(),
                ..LatestSnapshot::default()
            }),
            connections: AtomicUsize::new(0),
            answer_lock: Mutex::new(()),
        }
    }

    fn connected(&self) -> bool {
        self.connections.load(Ordering::Relaxed) > 0
    }
}

#[derive(Clone)]
struct AppState {
    config: Arc<CloudConfig>,
    devices: Arc<RwLock<HashMap<String, Arc<DeviceRuntime>>>>,
    store: Store,
    login_attempts: Arc<Mutex<HashMap<String, LoginAttempt>>>,
    dummy_password_hash: Arc<String>,
}

impl AppState {
    async fn device(&self, device_id: &str) -> Arc<DeviceRuntime> {
        if let Some(device) = self.devices.read().await.get(device_id).cloned() {
            return device;
        }
        let mut devices = self.devices.write().await;
        devices
            .entry(device_id.to_string())
            .or_insert_with(|| Arc::new(DeviceRuntime::new()))
            .clone()
    }
}

struct LoginAttempt {
    window_started: Instant,
    failures: u32,
}

#[derive(Deserialize)]
struct DeviceQuery {
    device_id: String,
}

#[derive(Serialize)]
struct ModelProfilesResponse {
    default_model_profile: String,
    profiles: Vec<crate::config::ModelProfile>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .init();

    let config = Arc::new(CloudConfig::from_env()?);
    let store = Store::open(&config.db_path)?;
    let admin = match store.admin_user()? {
        Some(admin) => admin,
        None => {
            let username = auth::validate_username(&config.admin_username)?;
            let password = config.bootstrap_admin_password.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "QA_ADMIN_PASSWORD is required because this database has no administrator"
                )
            })?;
            if std::env::var("QA_ADMIN_PASSWORD")
                .ok()
                .filter(|value| !value.is_empty())
                .is_none()
            {
                log::warn!(
                    "QA_WEB_TOKEN is deprecated; it is only being used once to bootstrap the initial admin password"
                );
            }
            let password_hash = auth::hash_password(password)?;
            let admin = store.create_initial_admin(&username, &password_hash)?;
            log::info!("created initial administrator '{}'", admin.username);
            admin
        }
    };
    store.provision_devices(config.device_tokens.keys(), &admin.id)?;
    store.cleanup_expired_auth_sessions()?;
    let dummy_password_hash = Arc::new(auth::hash_password("invalid-login-password")?);
    let state = AppState {
        config: config.clone(),
        devices: Arc::new(RwLock::new(HashMap::new())),
        store,
        login_attempts: Arc::new(Mutex::new(HashMap::new())),
        dummy_password_hash,
    };

    let index = config.web_dir.join("index.html");
    let static_files = ServeDir::new(&config.web_dir).fallback(ServeFile::new(index));
    let max_body = config.max_image_bytes.saturating_mul(2).max(1024 * 1024);

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/auth/login", post(login))
        .route("/v1/auth/logout", post(logout))
        .route("/v1/auth/me", get(auth_me))
        .route("/v1/admin/users", get(admin_state).post(admin_create_user))
        .route(
            "/v1/admin/users/:user_id/password",
            put(admin_reset_password),
        )
        .route(
            "/v1/admin/devices/:device_id/owner",
            put(admin_assign_device),
        )
        .route("/v1/model-profiles", get(model_profiles))
        .route(
            "/v1/model-profiles/:profile_id/test",
            post(model_profile_test),
        )
        .route("/v1/answers/stream", post(answer_stream))
        .route("/v1/turns/:turn_id/screenshot", get(turn_screenshot))
        .route("/v1/devices/connect", get(device_ws))
        .route("/v1/web/ws", get(web_ws))
        .fallback_service(static_files)
        .layer(DefaultBodyLimit::max(max_body))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    log::info!(
        "qa-api listening on http://{} with {} configured device(s)",
        config.bind_addr,
        config.device_tokens.len()
    );
    axum::serve(listener, app).await?;
    Ok(())
}

async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({"ok": true, "version": env!("CARGO_PKG_VERSION")}))
}

/// Lists only public profile names. The corresponding provider API keys remain
/// in the cloud process environment and are never returned to a desktop.
async fn model_profiles(
    State(state): State<AppState>,
    Query(query): Query<DeviceQuery>,
    headers: HeaderMap,
) -> Response {
    if !authorized_device(&state.config, &headers, &query.device_id) {
        return (StatusCode::UNAUTHORIZED, "invalid device credentials").into_response();
    }
    Json(ModelProfilesResponse {
        default_model_profile: state.config.default_model_profile.clone(),
        profiles: state.config.model_profiles.clone(),
    })
    .into_response()
}

/// Exercises the selected provider using a tiny image. This is intentionally a
/// real request so an invalid provider key/model/vision endpoint is detected;
/// callers should label it as a small billable model test.
async fn model_profile_test(
    State(state): State<AppState>,
    Path(profile_id): Path<String>,
    Query(query): Query<DeviceQuery>,
    headers: HeaderMap,
) -> Response {
    if !authorized_device(&state.config, &headers, &query.device_id) {
        return (StatusCode::UNAUTHORIZED, "invalid device credentials").into_response();
    }
    let mut cfg = match llm_config_for_profile(&state.config, &profile_id) {
        Ok(config) => config,
        Err(message) => return json_error(StatusCode::BAD_REQUEST, &message),
    };
    cfg.max_tokens = cfg.max_tokens.clamp(16, 32);
    if let Some(ocr) = cfg.ocr.as_mut() {
        ocr.max_tokens = ocr.max_tokens.clamp(16, 128);
    }
    // Some vision providers require both sides to be greater than 10 pixels.
    // Generate a tiny standard JPEG locally so this stays a cheap test without
    // transferring a real desktop screenshot or creating a history record.
    let (test_image, test_image_mime) = match model_test_image() {
        Ok(image) => image,
        Err(error) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("failed to prepare model test image: {error}"),
            )
        }
    };
    let result = tokio::time::timeout(Duration::from_secs(25), async {
        let mut stream = ai::stream_answer(
            &cfg,
            "这是连通性测试。请只回复 OK。",
            &test_image,
            test_image_mime,
        )
        .await?;
        match stream.next().await {
            Some(Ok(delta)) if !delta.trim().is_empty() => Ok::<String, anyhow::Error>(delta),
            Some(Err(error)) => Err(error),
            _ => Err(anyhow::anyhow!("model returned no streaming text")),
        }
    })
    .await;
    match result {
        Ok(Ok(sample)) => Json(serde_json::json!({
            "ok": true,
            "profileId": profile_id,
            "sample": sample,
        }))
        .into_response(),
        Ok(Err(error)) => json_error(
            StatusCode::BAD_GATEWAY,
            &format!("model test failed: {error}"),
        ),
        Err(_) => json_error(
            StatusCode::GATEWAY_TIMEOUT,
            "model test timed out after 25 seconds",
        ),
    }
}

fn model_test_image() -> anyhow::Result<(String, &'static str)> {
    use image::codecs::jpeg::JpegEncoder;

    let image = image::RgbImage::from_pixel(32, 32, image::Rgb([240, 240, 240]));
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 90)
        .encode(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(anyhow::Error::from)?;
    Ok((STANDARD.encode(jpeg), "image/jpeg"))
}

fn llm_config_for_profile(config: &CloudConfig, profile_id: &str) -> Result<ai::LlmConfig, String> {
    if !config.has_model_profile(profile_id) {
        return Err(format!("unknown model profile '{profile_id}'"));
    }
    if profile_id == "default" {
        ai::LlmConfig::from_env()
    } else {
        ai::LlmConfig::from_profile(Some(profile_id))
    }
    .map_err(|error| error.to_string())
}

async fn turn_screenshot(
    State(state): State<AppState>,
    Path(turn_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let user = match authenticated_user(&state, &headers) {
        Ok(user) => user,
        Err(response) => return *response,
    };
    let screenshot = match state.store.turn_screenshot(&turn_id) {
        Ok(Some(screenshot)) => screenshot,
        Ok(None) => return json_error(StatusCode::NOT_FOUND, "截图不存在"),
        Err(error) => {
            log::error!("failed to load turn screenshot: {error:#}");
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "无法读取截图");
        }
    };
    match state.store.can_access_device(&user, &screenshot.device_id) {
        Ok(true) => {}
        Ok(false) => return json_error(StatusCode::FORBIDDEN, "无权访问这张截图"),
        Err(error) => {
            log::error!("failed to authorize screenshot: {error:#}");
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "无法验证截图权限");
        }
    }
    let bytes = match STANDARD.decode(&screenshot.screenshot_b64) {
        Ok(bytes) => bytes,
        Err(error) => {
            log::error!("stored screenshot is invalid Base64: {error}");
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "截图数据损坏");
        }
    };
    let content_type = match screenshot.screenshot_mime.as_str() {
        "image/jpeg" => "image/jpeg",
        _ => "image/png",
    };
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "private, no-store"),
        ],
        bytes,
    )
        .into_response()
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Serialize)]
struct AuthView {
    user: AuthUser,
    devices: Vec<DeviceSummary>,
}

#[derive(Serialize)]
struct AdminView {
    users: Vec<AdminUserSummary>,
    devices: Vec<DeviceSummary>,
}

#[derive(Deserialize)]
struct CreateUserRequest {
    username: String,
    password: String,
}

#[derive(Deserialize)]
struct PasswordRequest {
    password: String,
}

#[derive(Deserialize)]
struct AssignDeviceRequest {
    user_id: String,
}

async fn login(State(state): State<AppState>, Json(request): Json<LoginRequest>) -> Response {
    let username = match auth::validate_username(&request.username) {
        Ok(username) => username,
        Err(_) => return login_failure(&state, request.username).await,
    };
    if request.password.chars().count() > auth::MAX_PASSWORD_CHARS {
        return login_failure(&state, username).await;
    }
    if login_blocked(&state, &username).await {
        return json_error(
            StatusCode::TOO_MANY_REQUESTS,
            "登录失败次数过多，请一分钟后重试",
        );
    }

    let secret = match state.store.user_secret_by_username(&username) {
        Ok(secret) => secret,
        Err(error) => {
            log::error!("failed to load login user: {error:#}");
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "登录服务暂不可用");
        }
    };
    let password = request.password;
    let encoded = secret
        .as_ref()
        .map(|secret| secret.password_hash.clone())
        .unwrap_or_else(|| (*state.dummy_password_hash).clone());
    let verified = tokio::task::spawn_blocking(move || auth::verify_password(&password, &encoded))
        .await
        .unwrap_or(false);
    let Some(secret) = secret.filter(|_| verified) else {
        return login_failure(&state, username).await;
    };

    state.login_attempts.lock().await.remove(&username);
    let token = auth::new_session_token();
    let token_hash = auth::session_token_hash(&token);
    let expires_at = unix_now().saturating_add(state.config.auth_session_seconds);
    if let Err(error) = state
        .store
        .create_auth_session(&token_hash, &secret.user.id, expires_at)
    {
        log::error!("failed to create login session: {error:#}");
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, "无法创建登录会话");
    }
    let view = match auth_view(&state, secret.user) {
        Ok(view) => view,
        Err(error) => {
            log::error!("failed to build login response: {error:#}");
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "无法加载账户信息");
        }
    };
    let mut response = Json(view).into_response();
    set_cookie_header(
        &mut response,
        auth::cookie_value(
            &token,
            state.config.auth_session_seconds,
            state.config.cookie_secure,
        ),
    );
    response
}

async fn login_failure(state: &AppState, username: String) -> Response {
    let key = username.trim().to_ascii_lowercase();
    let mut attempts = state.login_attempts.lock().await;
    let attempt = attempts.entry(key).or_insert(LoginAttempt {
        window_started: Instant::now(),
        failures: 0,
    });
    if attempt.window_started.elapsed() > Duration::from_secs(60) {
        attempt.window_started = Instant::now();
        attempt.failures = 0;
    }
    attempt.failures = attempt.failures.saturating_add(1);
    drop(attempts);
    tokio::time::sleep(Duration::from_millis(250)).await;
    json_error(StatusCode::UNAUTHORIZED, "用户名或密码错误")
}

async fn login_blocked(state: &AppState, username: &str) -> bool {
    let mut attempts = state.login_attempts.lock().await;
    let Some(attempt) = attempts.get_mut(username) else {
        return false;
    };
    if attempt.window_started.elapsed() > Duration::from_secs(60) {
        attempts.remove(username);
        return false;
    }
    attempt.failures >= 5
}

async fn auth_me(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let user = match authenticated_user(&state, &headers) {
        Ok(user) => user,
        Err(response) => return *response,
    };
    match auth_view(&state, user) {
        Ok(view) => Json(view).into_response(),
        Err(error) => {
            log::error!("failed to load current user: {error:#}");
            json_error(StatusCode::INTERNAL_SERVER_ERROR, "无法加载账户信息")
        }
    }
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = request_session_token(&headers) {
        let _ = state
            .store
            .delete_auth_session(&auth::session_token_hash(&token));
    }
    let mut response = Json(serde_json::json!({"ok": true})).into_response();
    set_cookie_header(
        &mut response,
        auth::clear_cookie_value(state.config.cookie_secure),
    );
    response
}

async fn admin_state(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let admin = match authenticated_admin(&state, &headers) {
        Ok(user) => user,
        Err(response) => return *response,
    };
    match build_admin_view(&state, &admin) {
        Ok(view) => Json(view).into_response(),
        Err(error) => {
            log::error!("failed to load admin state: {error:#}");
            json_error(StatusCode::INTERNAL_SERVER_ERROR, "无法加载用户管理数据")
        }
    }
}

async fn admin_create_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateUserRequest>,
) -> Response {
    let admin = match authenticated_admin(&state, &headers) {
        Ok(user) => user,
        Err(response) => return *response,
    };
    let username = match auth::validate_username(&request.username) {
        Ok(username) => username,
        Err(error) => return json_error(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let password_hash = match hash_password_async(request.password).await {
        Ok(hash) => hash,
        Err(error) => return json_error(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    if let Err(error) = state.store.create_user(&username, &password_hash) {
        return json_error(StatusCode::CONFLICT, &error.to_string());
    }
    match build_admin_view(&state, &admin) {
        Ok(view) => Json(view).into_response(),
        Err(error) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

async fn admin_reset_password(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<PasswordRequest>,
) -> Response {
    let admin = match authenticated_admin(&state, &headers) {
        Ok(user) => user,
        Err(response) => return *response,
    };
    let password_hash = match hash_password_async(request.password).await {
        Ok(hash) => hash,
        Err(error) => return json_error(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    if let Err(error) = state.store.set_user_password(&user_id, &password_hash) {
        return json_error(StatusCode::NOT_FOUND, &error.to_string());
    }
    let mut response = Json(serde_json::json!({"ok": true})).into_response();
    if user_id == admin.id {
        set_cookie_header(
            &mut response,
            auth::clear_cookie_value(state.config.cookie_secure),
        );
    }
    response
}

async fn admin_assign_device(
    State(state): State<AppState>,
    Path(device_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<AssignDeviceRequest>,
) -> Response {
    let admin = match authenticated_admin(&state, &headers) {
        Ok(user) => user,
        Err(response) => return *response,
    };
    if let Err(error) = state.store.assign_device(&device_id, &request.user_id) {
        return json_error(StatusCode::BAD_REQUEST, &error.to_string());
    }
    match build_admin_view(&state, &admin) {
        Ok(view) => Json(view).into_response(),
        Err(error) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

fn auth_view(state: &AppState, user: AuthUser) -> anyhow::Result<AuthView> {
    let devices = state.store.devices_for_user(&user)?;
    Ok(AuthView { user, devices })
}

async fn hash_password_async(password: String) -> anyhow::Result<String> {
    tokio::task::spawn_blocking(move || auth::hash_password(&password))
        .await
        .map_err(|error| anyhow::anyhow!("password hashing task failed: {error}"))?
}

fn build_admin_view(state: &AppState, admin: &AuthUser) -> anyhow::Result<AdminView> {
    Ok(AdminView {
        users: state.store.admin_users()?,
        devices: state.store.devices_for_user(admin)?,
    })
}

fn authenticated_admin(state: &AppState, headers: &HeaderMap) -> Result<AuthUser, Box<Response>> {
    let user = authenticated_user(state, headers)?;
    if !user.is_admin {
        return Err(Box::new(json_error(
            StatusCode::FORBIDDEN,
            "需要管理员权限",
        )));
    }
    Ok(user)
}

fn authenticated_user(state: &AppState, headers: &HeaderMap) -> Result<AuthUser, Box<Response>> {
    let Some(token) = request_session_token(headers) else {
        return Err(Box::new(json_error(StatusCode::UNAUTHORIZED, "请先登录")));
    };
    match state
        .store
        .user_by_auth_session(&auth::session_token_hash(&token))
    {
        Ok(Some(user)) => Ok(user),
        Ok(None) => Err(Box::new(json_error(
            StatusCode::UNAUTHORIZED,
            "登录已过期，请重新登录",
        ))),
        Err(error) => {
            log::error!("failed to authenticate web session: {error:#}");
            Err(Box::new(json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "认证服务暂不可用",
            )))
        }
    }
}

fn request_session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(auth::cookie_token)
        .map(str::to_string)
}

fn set_cookie_header(response: &mut Response, cookie: String) {
    if let Ok(value) = cookie.parse() {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({"error": message}))).into_response()
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

async fn answer_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AnswerRequest>,
) -> Response {
    if !authorized_device(&state.config, &headers, &request.device_id) {
        return (StatusCode::UNAUTHORIZED, "invalid device credentials").into_response();
    }
    if request.image_b64.len() > state.config.max_image_bytes.saturating_mul(4) / 3 + 16 {
        return (StatusCode::PAYLOAD_TOO_LARGE, "screenshot is too large").into_response();
    }
    if request.mime_type != "image/png" && request.mime_type != "image/jpeg" {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported screenshot type",
        )
            .into_response();
    }

    let stored_image = match ai::prepare_image(&request.image_b64, &request.mime_type) {
        Ok(image) => image,
        Err(error) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("invalid screenshot: {error}"),
            )
                .into_response();
        }
    };
    let device = state.device(&request.device_id).await;
    let session = match state
        .store
        .ensure_session(&request.device_id, request.session_id.as_deref())
    {
        Ok(session) => session,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, error.to_string()).into_response();
        }
    };
    let prompt = request
        .question
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&session.prompt)
        .to_string();
    let turn = match state.store.create_turn(
        &session.id,
        &prompt,
        &stored_image.base64,
        &stored_image.mime_type,
    ) {
        Ok(turn) => turn,
        Err(error) => {
            log::error!("failed to persist screenshot turn: {error:#}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to save screenshot",
            )
                .into_response();
        }
    };
    {
        let mut latest = device.latest.write().await;
        latest.screenshot_b64 = Some(stored_image.base64.clone());
        latest.screenshot_mime = Some(stored_image.mime_type.clone());
        latest.answer.clear();
        latest.status = "uploading".to_string();
    }
    let _ = device.events.send(QaEvent::Screenshot {
        image_b64: stored_image.base64.clone(),
        mime_type: stored_image.mime_type.clone(),
    });
    let _ = device.events.send(QaEvent::Uploading);
    publish_history(&state.store, &device, &request.device_id, &session.id);

    let store = state.store.clone();
    let device_id = request.device_id.clone();
    let session_id = session.id.clone();
    let turn_id = turn.id.clone();
    let image_b64 = stored_image.base64;
    let image_mime = stored_image.mime_type;
    let output = async_stream::stream! {
        let _guard = device.answer_lock.lock().await;
        yield Ok::<Event, Infallible>(sse_event(&QaEvent::Uploading));

        let profile_id = request
            .model_profile
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(&state.config.default_model_profile)
            .to_string();
        let cfg = match llm_config_for_profile(&state.config, &profile_id) {
            Ok(cfg) => cfg,
            Err(error) => {
                let message = format!("LLM config: {error}");
                let event = QaEvent::Error { message: message.clone() };
                publish(&device, &event).await;
                finish_history(&store, &device, &device_id, &session_id, &turn_id, "", &format!("error: {message}"));
                yield Ok(sse_event(&event));
                return;
            }
        };
        let mut llm_stream = match ai::stream_answer(&cfg, &prompt, &image_b64, &image_mime).await {
            Ok(stream) => stream,
            Err(error) => {
                let message = format!("LLM call failed: {error}");
                let event = QaEvent::Error { message: message.clone() };
                publish(&device, &event).await;
                finish_history(&store, &device, &device_id, &session_id, &turn_id, "", &format!("error: {message}"));
                yield Ok(sse_event(&event));
                return;
            }
        };

        let mut answer = String::new();
        while let Some(chunk) = llm_stream.next().await {
            match chunk {
                Ok(delta) => {
                    answer.push_str(&delta);
                    let event = QaEvent::Streaming { delta };
                    let _ = device.events.send(event.clone());
                    {
                        let mut latest = device.latest.write().await;
                        latest.answer = answer.clone();
                        latest.status = "streaming".to_string();
                    }
                    yield Ok(sse_event(&event));
                }
                Err(error) => {
                    let message = format!("LLM stream failed: {error}");
                    let event = QaEvent::Error { message: message.clone() };
                    publish(&device, &event).await;
                    finish_history(&store, &device, &device_id, &session_id, &turn_id, &answer, &format!("error: {message}"));
                    yield Ok(sse_event(&event));
                    return;
                }
            }
        }

        let event = QaEvent::Done { answer };
        publish(&device, &event).await;
        if let QaEvent::Done { answer } = &event {
            finish_history(&store, &device, &device_id, &session_id, &turn_id, answer, "done");
        }
        yield Ok(sse_event(&event));
    };

    Sse::new(output)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response()
}

async fn publish(device: &DeviceRuntime, event: &QaEvent) {
    {
        let mut latest = device.latest.write().await;
        match event {
            QaEvent::Done { answer } => {
                latest.answer = answer.clone();
                latest.status = "done".to_string();
            }
            QaEvent::Error { message } => latest.status = format!("error: {message}"),
            _ => {}
        }
    }
    let _ = device.events.send(event.clone());
}

fn sse_event(event: &QaEvent) -> Event {
    Event::default().data(
        serde_json::to_string(event)
            .unwrap_or_else(|_| r#"{"type":"Error","message":"serialization failed"}"#.to_string()),
    )
}

fn publish_history(store: &Store, device: &DeviceRuntime, device_id: &str, session_id: &str) {
    match store.session(device_id, session_id) {
        Ok(Some(session)) => match store.turns(session_id) {
            Ok(turns) => {
                let _ = device
                    .events
                    .send(QaEvent::SessionDetail { session, turns });
            }
            Err(error) => log::error!("failed to load session turns: {error:#}"),
        },
        Ok(None) => {}
        Err(error) => log::error!("failed to load session: {error:#}"),
    }
    match store.list_sessions(device_id) {
        Ok(sessions) => {
            let _ = device.events.send(QaEvent::SessionList {
                sessions,
                active_session_id: None,
            });
        }
        Err(error) => log::error!("failed to list sessions: {error:#}"),
    }
}

fn finish_history(
    store: &Store,
    device: &DeviceRuntime,
    device_id: &str,
    session_id: &str,
    turn_id: &str,
    answer: &str,
    status: &str,
) {
    if let Err(error) = store.finish_turn(turn_id, answer, status) {
        log::error!("failed to persist turn result: {error:#}");
    }
    publish_history(store, device, device_id, session_id);
}

async fn device_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<DeviceQuery>,
    headers: HeaderMap,
) -> Response {
    if !authorized_device(&state.config, &headers, &query.device_id) {
        return (StatusCode::UNAUTHORIZED, "invalid device credentials").into_response();
    }
    let device = state.device(&query.device_id).await;
    ws.on_upgrade(move |socket| handle_device_ws(socket, device))
}

async fn handle_device_ws(socket: WebSocket, device: Arc<DeviceRuntime>) {
    let previous = device.connections.fetch_add(1, Ordering::Relaxed);
    if previous == 0 {
        let _ = device
            .events
            .send(QaEvent::DeviceStatus { connected: true });
    }

    let (mut sender, mut receiver) = socket.split();
    let mut commands = device.commands.subscribe();
    let mut send_task = tokio::spawn(async move {
        while let Ok(command) = commands.recv().await {
            let Ok(text) = serde_json::to_string(&command) else {
                continue;
            };
            if sender.send(Message::Text(text)).await.is_err() {
                break;
            }
        }
    });
    let report_device = device.clone();
    let mut receive_task = tokio::spawn(async move {
        while let Some(Ok(message)) = receiver.next().await {
            match message {
                Message::Text(text) => {
                    if let Ok(event @ QaEvent::Error { .. }) =
                        serde_json::from_str::<QaEvent>(&text)
                    {
                        publish(&report_device, &event).await;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = &mut send_task => receive_task.abort(),
        _ = &mut receive_task => send_task.abort(),
    }

    let previous = device.connections.fetch_sub(1, Ordering::Relaxed);
    if previous <= 1 {
        let _ = device
            .events
            .send(QaEvent::DeviceStatus { connected: false });
    }
}

async fn web_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<DeviceQuery>,
    headers: HeaderMap,
) -> Response {
    let user = match authenticated_user(&state, &headers) {
        Ok(user) => user,
        Err(response) => return *response,
    };
    match state.store.can_access_device(&user, &query.device_id) {
        Ok(true) => {}
        Ok(false) => return (StatusCode::FORBIDDEN, "device access denied").into_response(),
        Err(error) => {
            log::error!("failed to authorize web device access: {error:#}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "authorization failed").into_response();
        }
    }
    let device_id = query.device_id;
    let device = state.device(&device_id).await;
    ws.on_upgrade(move |socket| handle_web_ws(socket, state, device, device_id))
}

async fn handle_web_ws(
    mut socket: WebSocket,
    state: AppState,
    device: Arc<DeviceRuntime>,
    device_id: String,
) {
    let active_session = match state.store.ensure_session(&device_id, None) {
        Ok(session) => session,
        Err(error) => {
            let _ = send_web_event(
                &mut socket,
                &QaEvent::Error {
                    message: format!("failed to initialize session: {error}"),
                },
            )
            .await;
            return;
        }
    };

    let snapshot = device.latest.read().await.clone();
    let initial = QaEvent::Snapshot {
        connected: device.connected(),
        screenshot_b64: snapshot.screenshot_b64,
        screenshot_mime: snapshot.screenshot_mime,
        answer: snapshot.answer,
        status: snapshot.status,
    };
    if send_web_event(&mut socket, &initial).await.is_err() {
        return;
    }
    if let Ok(event) = session_list_event(&state.store, &device_id, Some(&active_session.id)) {
        if send_web_event(&mut socket, &event).await.is_err() {
            return;
        }
    }
    if let Ok(event) = session_detail_event(&state.store, &device_id, &active_session.id) {
        if send_web_event(&mut socket, &event).await.is_err() {
            return;
        }
    }

    let (mut sender, mut receiver) = socket.split();
    let mut events = device.events.subscribe();
    let (direct_tx, mut direct_rx) = mpsc::channel::<QaEvent>(32);
    let mut send_task = tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                event = events.recv() => match event {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                event = direct_rx.recv() => match event {
                    Some(event) => event,
                    None => break,
                }
            };
            let Ok(text) = serde_json::to_string(&event) else {
                continue;
            };
            if sender.send(Message::Text(text)).await.is_err() {
                break;
            }
        }
    });
    let command_device = device.clone();
    let command_store = state.store.clone();
    let command_device_id = device_id.clone();
    let mut receive_task = tokio::spawn(async move {
        let mut active_session_id = active_session.id;
        while let Some(Ok(message)) = receiver.next().await {
            let Message::Text(text) = message else {
                continue;
            };
            let Ok(message) = serde_json::from_str::<WebClientMessage>(&text) else {
                let _ = direct_tx
                    .send(QaEvent::Error {
                        message: "invalid web command".to_string(),
                    })
                    .await;
                continue;
            };
            match message {
                WebClientMessage::Trigger {
                    question,
                    session_id,
                } => {
                    let selected_id = session_id.unwrap_or_else(|| active_session_id.clone());
                    let session = match command_store.session(&command_device_id, &selected_id) {
                        Ok(Some(session)) => session,
                        _ => {
                            let _ = direct_tx
                                .send(QaEvent::Error {
                                    message: "会话不存在，请重新选择".to_string(),
                                })
                                .await;
                            continue;
                        }
                    };
                    active_session_id = session.id.clone();
                    let prompt = question
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| session.prompt.clone());
                    let _ = command_store.update_session(
                        &command_device_id,
                        &session.id,
                        None,
                        Some(&prompt),
                    );
                    if command_device
                        .commands
                        .send(DeviceCommand::Trigger {
                            question: Some(prompt),
                            session_id: Some(session.id),
                        })
                        .is_err()
                    {
                        let _ = direct_tx
                            .send(QaEvent::Error {
                                message: "桌面设备离线，无法截图".to_string(),
                            })
                            .await;
                    } else {
                        let _ = command_device.events.send(QaEvent::Capturing);
                    }
                }
                WebClientMessage::CreateSession { title, prompt } => {
                    match command_store.create_session(
                        &command_device_id,
                        title.as_deref(),
                        prompt.as_deref(),
                    ) {
                        Ok(session) => {
                            active_session_id = session.id.clone();
                            queue_session_state(
                                &direct_tx,
                                &command_store,
                                &command_device_id,
                                &active_session_id,
                            )
                            .await;
                        }
                        Err(error) => {
                            let _ = direct_tx
                                .send(QaEvent::Error {
                                    message: format!("新建会话失败：{error}"),
                                })
                                .await;
                        }
                    }
                }
                WebClientMessage::SelectSession { session_id } => {
                    if command_store
                        .session(&command_device_id, &session_id)
                        .ok()
                        .flatten()
                        .is_some()
                    {
                        active_session_id = session_id;
                        queue_session_state(
                            &direct_tx,
                            &command_store,
                            &command_device_id,
                            &active_session_id,
                        )
                        .await;
                    }
                }
                WebClientMessage::UpdateSession {
                    session_id,
                    title,
                    prompt,
                } => match command_store.update_session(
                    &command_device_id,
                    &session_id,
                    title.as_deref(),
                    prompt.as_deref(),
                ) {
                    Ok(_) => {
                        active_session_id = session_id;
                        queue_session_state(
                            &direct_tx,
                            &command_store,
                            &command_device_id,
                            &active_session_id,
                        )
                        .await;
                    }
                    Err(error) => {
                        let _ = direct_tx
                            .send(QaEvent::Error {
                                message: format!("保存会话失败：{error}"),
                            })
                            .await;
                    }
                },
                WebClientMessage::Ping => {
                    let _ = direct_tx
                        .send(QaEvent::DeviceStatus {
                            connected: command_device.connected(),
                        })
                        .await;
                }
            }
        }
    });

    tokio::select! {
        _ = &mut send_task => receive_task.abort(),
        _ = &mut receive_task => send_task.abort(),
    }
}

async fn send_web_event(socket: &mut WebSocket, event: &QaEvent) -> Result<(), axum::Error> {
    socket
        .send(Message::Text(
            serde_json::to_string(event).unwrap_or_default(),
        ))
        .await
}

fn session_list_event(
    store: &Store,
    device_id: &str,
    active_session_id: Option<&str>,
) -> anyhow::Result<QaEvent> {
    Ok(QaEvent::SessionList {
        sessions: store.list_sessions(device_id)?,
        active_session_id: active_session_id.map(str::to_string),
    })
}

fn session_detail_event(
    store: &Store,
    device_id: &str,
    session_id: &str,
) -> anyhow::Result<QaEvent> {
    let session = store
        .session(device_id, session_id)?
        .ok_or_else(|| anyhow::anyhow!("unknown session"))?;
    let mut turns = store.turns(session_id)?;
    // History screenshots are fetched lazily through an authenticated HTTP
    // endpoint. Keeping Base64 blobs out of the WebSocket prevents a mobile
    // client from receiving one multi-megabyte frame for every session load.
    for turn in &mut turns {
        turn.screenshot_b64.clear();
    }
    Ok(QaEvent::SessionDetail { session, turns })
}

async fn queue_session_state(
    sender: &mpsc::Sender<QaEvent>,
    store: &Store,
    device_id: &str,
    session_id: &str,
) {
    match session_list_event(store, device_id, Some(session_id)) {
        Ok(event) => {
            let _ = sender.send(event).await;
        }
        Err(error) => log::error!("failed to build session list: {error:#}"),
    }
    match session_detail_event(store, device_id, session_id) {
        Ok(event) => {
            let _ = sender.send(event).await;
        }
        Err(error) => log::error!("failed to build session detail: {error:#}"),
    }
}

fn authorized_device(config: &CloudConfig, headers: &HeaderMap, device_id: &str) -> bool {
    let Some(expected) = config.device_token(device_id) else {
        return false;
    };
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    value.strip_prefix("Bearer ") == Some(expected)
}

#[cfg(test)]
mod tests {
    use super::{model_test_image, session_detail_event};
    use crate::store::Store;
    use qa_protocol::QaEvent;

    #[test]
    fn model_test_image_is_a_valid_32px_jpeg() {
        use base64::Engine as _;
        use image::GenericImageView;

        let (image_b64, mime) = model_test_image().expect("create model test image");
        assert_eq!(mime, "image/jpeg");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(image_b64)
            .expect("decode JPEG");
        let image = image::load_from_memory(&bytes).expect("decode JPEG image");
        assert_eq!(image.dimensions(), (32, 32));
    }

    #[test]
    fn websocket_session_history_omits_inline_screenshot_data() {
        let path =
            std::env::temp_dir().join(format!("qa-session-event-test-{}.db", uuid::Uuid::new_v4()));
        let store = Store::open(&path).expect("open store");
        let session = store
            .create_session("desktop-1", Some("移动端"), Some("分析页面"))
            .expect("create session");
        store
            .create_turn(
                &session.id,
                "分析页面",
                "large-base64-placeholder",
                "image/png",
            )
            .expect("create turn");

        let event =
            session_detail_event(&store, "desktop-1", &session.id).expect("build session detail");
        let QaEvent::SessionDetail { turns, .. } = event else {
            panic!("expected session detail");
        };
        assert_eq!(turns.len(), 1);
        assert!(turns[0].screenshot_b64.is_empty());
        assert_eq!(
            store
                .turn_screenshot(&turns[0].id)
                .expect("load screenshot")
                .expect("screenshot exists")
                .screenshot_b64,
            "large-base64-placeholder"
        );

        drop(store);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
    }
}
