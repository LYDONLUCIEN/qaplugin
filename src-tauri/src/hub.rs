use qa_protocol::QaEvent;
use tauri::{Emitter, Manager};
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct LocalEventState {
    tx: broadcast::Sender<QaEvent>,
}

pub fn init(app: &tauri::AppHandle) {
    let (tx, _) = broadcast::channel::<QaEvent>(256);
    app.manage(LocalEventState { tx: tx.clone() });

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut events = tx.subscribe();
        while let Ok(event) = events.recv().await {
            let _ = app.emit("hub", event);
        }
    });
}

pub fn emit(app: &tauri::AppHandle, event: QaEvent) {
    if let Some(state) = app.try_state::<LocalEventState>() {
        let _ = state.tx.send(event);
    }
}
