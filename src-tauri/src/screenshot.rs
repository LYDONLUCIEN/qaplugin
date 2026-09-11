use futures_util::StreamExt;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use qa_protocol::QaEvent;
use std::time::Duration;
use tauri::Manager;
use tokio::sync::Mutex;

static CAPTURE_LOCK: Mutex<()> = Mutex::const_new(());

// A Retina full-screen PNG is often 8–20 MB. Compress locally before Base64
// expands it for the public upload, while preserving enough pixels for UI text.
const COMPRESS_ABOVE_BYTES: usize = 1_500_000;
const MAX_UPLOAD_SIDE: u32 = 2048;
const JPEG_QUALITY: u8 = 82;

pub(crate) struct UploadImage {
    pub bytes: Vec<u8>,
    pub mime_type: &'static str,
}

pub async fn capture_and_ask(
    app: tauri::AppHandle,
    question: Option<String>,
    session_id: Option<String>,
    model_profile: Option<String>,
) -> Result<String, String> {
    let _guard = CAPTURE_LOCK
        .try_lock()
        .map_err(|_| fail(&app, "已有截图问答正在处理中，请等待完成后再试".to_string()))?;
    crate::hub::emit(&app, QaEvent::Capturing);

    if let Some(window) = app.get_webview_window("overlay") {
        let _ = window.show();
    }

    let image = crate::platform::capture_to_bytes()
        .await
        .map_err(|error| fail(&app, format!("capture failed: {error}")))?;
    let image = compress_for_upload(image)
        .map_err(|error| fail(&app, format!("screenshot compression failed: {error}")))?;
    crate::hub::emit(&app, QaEvent::Uploading);

    let config = crate::cloud::current(&app)
        .ok_or_else(|| fail(&app, "cloud config is not set".to_string()))?;
    let mut events = tokio::time::timeout(
        Duration::from_secs(120),
        crate::cloud::stream_answer(
            &config,
            &image.bytes,
            image.mime_type,
            question,
            session_id,
            model_profile.as_deref(),
        ),
    )
    .await
    .map_err(|_| {
        fail(
            &app,
            "cloud request timed out after 120 seconds".to_string(),
        )
    })?
    .map_err(|error| fail(&app, format!("cloud request failed: {error}")))?;

    let mut answer = String::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(180), events.next())
            .await
            .map_err(|_| {
                fail(
                    &app,
                    "cloud answer stream timed out after 180 seconds".to_string(),
                )
            })?;
        let Some(event) = event else {
            break;
        };
        let event = event.map_err(|error| fail(&app, format!("cloud stream failed: {error}")))?;
        match &event {
            QaEvent::Streaming { delta } => answer.push_str(delta),
            QaEvent::Done {
                answer: final_answer,
                ..
            } => answer = final_answer.clone(),
            QaEvent::Error { message } => {
                crate::hub::emit(&app, event.clone());
                return Err(message.clone());
            }
            _ => {}
        }
        crate::hub::emit(&app, event);
    }

    if answer.is_empty() {
        return Err(fail(
            &app,
            "cloud stream ended without an answer".to_string(),
        ));
    }
    Ok(answer)
}

fn compress_for_upload(png: Vec<u8>) -> anyhow::Result<UploadImage> {
    if png.len() <= COMPRESS_ABOVE_BYTES {
        return Ok(UploadImage {
            bytes: png,
            mime_type: "image/png",
        });
    }

    let original_len = png.len();
    let original = image::load_from_memory(&png)?;
    let resized = original.resize(MAX_UPLOAD_SIDE, MAX_UPLOAD_SIDE, FilterType::Lanczos3);
    let rgb = resized.to_rgb8();
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY).encode(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        image::ColorType::Rgb8.into(),
    )?;

    // Avoid making already efficient PNG screenshots worse.
    if jpeg.len() * 100 < original_len * 85 {
        log::info!(
            "compressed upload screenshot from {} to {} bytes ({}x{}, JPEG q{})",
            original_len,
            jpeg.len(),
            rgb.width(),
            rgb.height(),
            JPEG_QUALITY
        );
        return Ok(UploadImage {
            bytes: jpeg,
            mime_type: "image/jpeg",
        });
    }
    log::info!(
        "kept PNG upload screenshot at {} bytes; JPEG would be {} bytes",
        original_len,
        jpeg.len()
    );
    Ok(UploadImage {
        bytes: png,
        mime_type: "image/png",
    })
}

fn fail(app: &tauri::AppHandle, message: String) -> String {
    crate::hub::emit(
        app,
        QaEvent::Error {
            message: message.clone(),
        },
    );
    message
}
