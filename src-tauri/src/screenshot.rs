use futures_util::StreamExt;
use qa_protocol::QaEvent;
use tauri::Manager;
use tokio::sync::Mutex;

static CAPTURE_LOCK: Mutex<()> = Mutex::const_new(());

pub async fn capture_and_ask(
    app: tauri::AppHandle,
    question: Option<String>,
    session_id: Option<String>,
) -> Result<String, String> {
    let _guard = CAPTURE_LOCK.lock().await;
    crate::hub::emit(&app, QaEvent::Capturing);

    if let Some(window) = app.get_webview_window("overlay") {
        let _ = window.show();
    }

    let image = crate::platform::capture_to_bytes()
        .await
        .map_err(|error| fail(&app, format!("capture failed: {error}")))?;
    crate::hub::emit(&app, QaEvent::Uploading);

    let config = crate::cloud::current(&app)
        .ok_or_else(|| fail(&app, "cloud config is not set".to_string()))?;
    let mut events = crate::cloud::stream_answer(&config, &image, question, session_id)
        .await
        .map_err(|error| fail(&app, format!("cloud request failed: {error}")))?;

    let mut answer = String::new();
    while let Some(event) = events.next().await {
        let event = event.map_err(|error| fail(&app, format!("cloud stream failed: {error}")))?;
        match &event {
            QaEvent::Streaming { delta } => answer.push_str(delta),
            QaEvent::Done {
                answer: final_answer,
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

fn fail(app: &tauri::AppHandle, message: String) -> String {
    crate::hub::emit(
        app,
        QaEvent::Error {
            message: message.clone(),
        },
    );
    message
}
