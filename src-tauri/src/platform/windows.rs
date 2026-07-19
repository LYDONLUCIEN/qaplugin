use anyhow::Result;
use tauri::WebviewWindow;

pub async fn capture_to_bytes() -> Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;

    let path = std::env::temp_dir().join(format!("qa-snap-{}.png", uuid::Uuid::new_v4()));
    let escaped_path = path.to_string_lossy().replace('\'', "''");
    let script = format!(
        r#"
Add-Type -AssemblyName System.Drawing,System.Windows.Forms
$bounds = [System.Windows.Forms.SystemInformation]::VirtualScreen
$bitmap = New-Object System.Drawing.Bitmap($bounds.Width, $bounds.Height)
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$graphics.CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bounds.Size)
$bitmap.Save('{escaped_path}', [System.Drawing.Imaging.ImageFormat]::Png)
$graphics.Dispose()
$bitmap.Dispose()
"#
    );
    let status = tokio::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status()
        .await?;
    if !status.success() {
        anyhow::bail!("PowerShell screenshot exited {status}");
    }

    let mut file = tokio::fs::File::open(&path).await?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await?;
    let _ = tokio::fs::remove_file(&path).await;
    if bytes.is_empty() {
        anyhow::bail!("empty screenshot");
    }
    Ok(bytes)
}

pub fn set_window_protected(window: &WebviewWindow, protected: bool) -> Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
    };

    let hwnd = window.hwnd()?;
    let affinity = if protected {
        WDA_EXCLUDEFROMCAPTURE
    } else {
        WDA_NONE
    };
    unsafe { SetWindowDisplayAffinity(hwnd, affinity)? };
    log::info!(
        "Windows: overlay anti-capture {}",
        if protected { "ON" } else { "OFF" }
    );
    Ok(())
}

pub fn refresh(_window: &WebviewWindow) -> Result<()> {
    Ok(())
}
