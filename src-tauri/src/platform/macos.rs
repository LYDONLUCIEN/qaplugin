use anyhow::Result;
use tauri::{Emitter, WebviewWindow};

pub async fn capture_to_bytes() -> Result<Vec<u8>> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;

    let tmp = std::env::temp_dir().join(format!("qa-snap-{}.png", uuid::Uuid::new_v4()));
    let path = tmp.to_string_lossy().to_string();
    let status = tokio::process::Command::new("/usr/sbin/screencapture")
        .args(["-x", "-C", &path])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?;
    if !status.success() {
        anyhow::bail!("screencapture exited {status}");
    }

    let mut file = tokio::fs::File::open(&tmp).await?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await?;
    let _ = tokio::fs::remove_file(&tmp).await;
    if bytes.is_empty() {
        anyhow::bail!("empty screenshot");
    }
    Ok(bytes)
}

pub fn set_window_protected(window: &WebviewWindow, protected: bool) -> Result<()> {
    use objc2::msg_send;
    use objc2_app_kit::NSWindow;

    let raw = window.ns_window()? as *mut NSWindow;
    if raw.is_null() {
        anyhow::bail!("null NSWindow");
    }
    let sharing: usize = if protected { 0 } else { 1 };

    unsafe {
        let _: () = msg_send![&*raw, setSharingType: sharing];
        let level: i64 = 25;
        let _: () = msg_send![&*raw, setLevel: level];
        let behavior: usize = 1usize | (1usize << 3);
        let _: () = msg_send![&*raw, setCollectionBehavior: behavior];
    }

    log::info!(
        "macOS: overlay anti-capture {}",
        if protected { "ON" } else { "OFF" }
    );
    Ok(())
}

pub fn refresh(window: &WebviewWindow) -> Result<()> {
    let _ = window.emit("screen-captured", is_screen_being_captured());
    Ok(())
}

fn is_screen_being_captured() -> bool {
    use core_foundation::base::{CFTypeRef, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;

    extern "C" {
        fn CGSessionCopyCurrentDictionary() -> CFTypeRef;
    }

    unsafe {
        let dict_ptr = CGSessionCopyCurrentDictionary();
        if dict_ptr.is_null() {
            return false;
        }
        let dict: CFDictionary<CFString, CFTypeRef> =
            CFDictionary::wrap_under_create_rule(dict_ptr as *const _);
        let key = CFString::new("kCGSessionScreenIsCapturedKey");
        dict.find(&key)
            .map(|value| {
                CFBoolean::wrap_under_get_rule(*value as *const _) == CFBoolean::true_value()
            })
            .unwrap_or(false)
    }
}
