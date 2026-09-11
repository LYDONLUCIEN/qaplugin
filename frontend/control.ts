// Control panel logic: manages configuration, capture controls and status.
// Answers are intentionally rendered only by the Overlay and cloud web app.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";

interface HubEvent {
  type: "Capturing" | "Uploading" | "Streaming" | "Done" | "Error";
  delta?: string;
  answer?: string;
  message?: string;
}

interface ModelProfile {
  id: string;
  label: string;
}

interface ModelProfilesResponse {
  defaultModelProfile: string;
  profiles: ModelProfile[];
}

function setStatus(text: string) {
  const el = document.getElementById("status")!;
  el.textContent = text;
}

let configLoaded = false;

function modelProfileSelect() {
  return document.getElementById("model-profile") as HTMLSelectElement;
}

function setModelProfiles(profiles: ModelProfile[], selected: string, defaultProfile: string) {
  const select = modelProfileSelect();
  select.innerHTML = "";
  for (const profile of profiles) {
    const option = document.createElement("option");
    option.value = profile.id;
    option.textContent = profile.id === defaultProfile
      ? `${profile.label}（服务器默认）`
      : profile.label;
    select.appendChild(option);
  }
  const wanted = selected || defaultProfile;
  if (wanted && [...select.options].some((option) => option.value === wanted)) {
    select.value = wanted;
  }
}

function setSavedModelProfile(profileId: string) {
  if (!profileId) return;
  const select = modelProfileSelect();
  if (![...select.options].some((option) => option.value === profileId)) {
    const option = document.createElement("option");
    option.value = profileId;
    option.textContent = `${profileId}（已保存；请测试刷新列表）`;
    select.appendChild(option);
  }
  select.value = profileId;
}

async function refreshStatus() {
  const s = await invoke<any>("get_status");
  if (s.configError) {
    setStatus("配置错误: " + s.configError);
    document.getElementById("url")!.textContent = "—";
    return;
  }
  if (!configLoaded) {
    (document.getElementById("cloud-url") as HTMLInputElement).value = String(s.cloudUrl || "");
    (document.getElementById("web-url") as HTMLInputElement).value = String(s.webUrl || "");
    (document.getElementById("device-id-config") as HTMLInputElement).value = String(s.deviceId || "");
    setSavedModelProfile(String(s.modelProfile || ""));
    configLoaded = true;
  }
  const url = String(s.phoneUrl || "");
  document.getElementById("url")!.textContent = url;
  document.getElementById("device")!.textContent = String(s.deviceId || "—");
  const profile = String(s.modelProfile || "");
  setStatus(s.cloudConnected ? `云端已连接${profile ? ` · ${profile}` : ""}` : "云端重连中…");
  await renderQR(url);
}

let renderedQR = "";

async function renderQR(text: string) {
  if (!text || text === renderedQR) return;
  renderedQR = text;
  const el = document.getElementById("qr")!;
  el.innerHTML = "";
  const img = document.createElement("img");
  img.alt = "qr";
  img.src = `https://api.qrserver.com/v1/create-qr-code/?size=200x200&data=${encodeURIComponent(text)}`;
  el.appendChild(img);
}

async function main() {
  let unlisten: UnlistenFn | undefined;

  try {
    const version = await invoke<string>("get_app_version");
    document.getElementById("app-version")!.textContent = `v${version}`;
  } catch (error) {
    console.warn("could not read app version", error);
  }

  try {
    unlisten = await listen<HubEvent>("hub", (e) => {
      const ev = e.payload;
      switch (ev.type) {
        case "Capturing":
          setStatus("capturing…");
          break;
        case "Uploading":
          setStatus("云端分析中…");
          break;
        case "Streaming":
          setStatus("云端正在流式回答…");
          break;
        case "Done":
          setStatus("done");
          break;
        case "Error":
          setStatus("error: " + (ev.message ?? ""));
          break;
      }
    });
  } catch (e) {
    console.warn("listen failed", e);
  }

  document.getElementById("capture")?.addEventListener("click", async () => {
    const q = (document.getElementById("q") as HTMLTextAreaElement).value.trim();
    setStatus("capturing…");
    try {
      await invoke("trigger_capture", { question: q || null });
    } catch (e) {
      setStatus("error: " + e);
    }
  });

  document.getElementById("toggle")?.addEventListener("click", async () => {
    await invoke("toggle_overlay_visible");
  });

  document.getElementById("protect")?.addEventListener("change", async (e) => {
    const checked = (e.target as HTMLInputElement).checked;
    await invoke("set_overlay_protected", { protected: checked });
  });

  document.getElementById("save-config")?.addEventListener("click", async () => {
    const cloudUrl = (document.getElementById("cloud-url") as HTMLInputElement).value.trim();
    const webUrlInput = (document.getElementById("web-url") as HTMLInputElement).value.trim();
    const deviceId = (document.getElementById("device-id-config") as HTMLInputElement).value.trim();
    const deviceToken = (document.getElementById("device-token") as HTMLInputElement).value.trim();
    const modelProfile = modelProfileSelect().value;
    setStatus("正在保存配置…");
    try {
      await invoke("save_cloud_config", {
        cloudUrl,
        webUrl: webUrlInput || cloudUrl,
        deviceId,
        deviceToken: deviceToken || null,
        modelProfile: modelProfile || null,
      });
      (document.getElementById("device-token") as HTMLInputElement).value = "";
      configLoaded = false;
      await refreshStatus();
    } catch (error) {
      setStatus("配置保存失败: " + error);
    }
  });

  document.getElementById("test-cloud")?.addEventListener("click", async () => {
    const cloudUrl = (document.getElementById("cloud-url") as HTMLInputElement).value.trim();
    const deviceId = (document.getElementById("device-id-config") as HTMLInputElement).value.trim();
    const deviceToken = (document.getElementById("device-token") as HTMLInputElement).value.trim();
    const currentProfile = modelProfileSelect().value;
    setStatus("正在测试云端、设备 Token 与模型列表…");
    try {
      const result = await invoke<ModelProfilesResponse>("test_cloud_connection", {
        cloudUrl,
        deviceId,
        deviceToken: deviceToken || null,
      });
      setModelProfiles(result.profiles, currentProfile, result.defaultModelProfile);
      setStatus(`测试成功：发现 ${result.profiles.length} 个模型链路`);
    } catch (error) {
      setStatus("测试失败: " + error);
    }
  });

  document.getElementById("test-model")?.addEventListener("click", async () => {
    const cloudUrl = (document.getElementById("cloud-url") as HTMLInputElement).value.trim();
    const deviceId = (document.getElementById("device-id-config") as HTMLInputElement).value.trim();
    const deviceToken = (document.getElementById("device-token") as HTMLInputElement).value.trim();
    const modelProfile = modelProfileSelect().value;
    if (!modelProfile) {
      setStatus("请先测试云端并选择模型链路");
      return;
    }
    setStatus(`正在测试 ${modelProfile}（会消耗少量 Token）…`);
    try {
      const result = await invoke<{ profileId: string; sample: string }>("test_model_profile", {
        cloudUrl,
        deviceId,
        deviceToken: deviceToken || null,
        modelProfile,
      });
      setStatus(`模型测试成功 · ${result.profileId}: ${result.sample}`);
    } catch (error) {
      setStatus("模型测试失败: " + error);
    }
  });

  document.getElementById("copy")?.addEventListener("click", async () => {
    const text = document.getElementById("url")!.textContent ?? "";
    try { await navigator.clipboard.writeText(text); } catch { /* may be blocked */ }
  });

  document.getElementById("open")?.addEventListener("click", async () => {
    const text = document.getElementById("url")!.textContent ?? "";
    if (text) await openUrl(text);
  });

  // Initial render + keep cloud connection status fresh.
  await refreshStatus();
  const statusTimer = window.setInterval(() => void refreshStatus(), 2000);

  // Re-bind on window close.
  window.addEventListener("beforeunload", () => {
    window.clearInterval(statusTimer);
    unlisten?.();
  });
}

main();
