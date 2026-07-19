use anyhow::Result;
use tauri::WebviewWindow;

pub async fn capture_to_bytes() -> Result<Vec<u8>> {
    anyhow::bail!("Linux screenshot is not implemented")
}

pub fn set_window_protected(_window: &WebviewWindow, _protected: bool) -> Result<()> {
    log::warn!("Linux anti-capture is not implemented");
    Ok(())
}

pub fn refresh(_window: &WebviewWindow) -> Result<()> {
    Ok(())
}
