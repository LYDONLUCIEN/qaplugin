// Overlay logic: listens to hub events + the macOS "screen-captured"
// signal from anti-capture polling. When recording is detected the
// content gets blanked (placeholder shown); when it stops the latest
// answer re-appears.

import { listen } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { renderMarkdown } from "./markdown";

interface HubEvent {
  type: "Capturing" | "Uploading" | "Streaming" | "Done" | "Error";
  delta?: string;
  answer?: string;
  message?: string;
}

const win = getCurrentWebviewWindow();

function setState(text: string, cls: "idle" | "busy" | "err") {
  const dot = document.querySelector(".dot") as HTMLElement;
  dot.classList.remove("busy", "err");
  if (cls === "busy") dot.classList.add("busy");
  if (cls === "err") dot.classList.add("err");
  document.getElementById("state")!.textContent = text;
}

function setContent(html: string) {
  const content = document.getElementById("content")!;
  content.classList.remove("markdown-body");
  content.innerHTML = html;
}

let lastAnswer = "";

function renderAnswer(text: string) {
  lastAnswer = text;
  if (recordingDetected) return; // will re-render when capture stops
  const content = document.getElementById("content")!;
  content.classList.add("markdown-body");
  renderMarkdown(content, text);
  content.scrollTop = content.scrollHeight;
}

function escapeHtml(s: string) {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

let recordingDetected = false;

async function main() {
  let pinned = localStorage.getItem("qa-overlay-pinned") !== "false";
  const pinButton = document.getElementById("pin") as HTMLButtonElement;
  const renderPin = () => {
    pinButton.textContent = pinned ? "已钉住" : "未钉住";
    pinButton.setAttribute("aria-pressed", String(pinned));
    pinButton.classList.toggle("active", pinned);
  };
  renderPin();
  await win.setAlwaysOnTop(pinned);

  // Hub events
  await listen<HubEvent>("hub", (e) => {
    const ev = e.payload as HubEvent;
    switch (ev.type) {
      case "Capturing":
        lastAnswer = "";
        setState("capturing", "busy");
        setContent(`<p class="placeholder">截屏中…</p>`);
        break;
      case "Uploading":
        lastAnswer = "";
        setState("thinking", "busy");
        setContent(`<p class="placeholder">已上传云端，模型分析中…</p>`);
        break;
      case "Streaming":
        setState("streaming", "busy");
        lastAnswer += ev.delta ?? "";
        renderAnswer(lastAnswer);
        break;
      case "Done":
        setState("done", "idle");
        renderAnswer(ev.answer ?? lastAnswer);
        break;
      case "Error":
        setState("err: " + (ev.message ?? ""), "err");
        setContent(`<p class="placeholder">⚠ ${escapeHtml(ev.message ?? "")}</p>`);
        break;
    }
  });

  // macOS screen-recording detected by polling CGSessionCopyDictionary
  await listen<boolean>("screen-captured", (e) => {
    recordingDetected = !!e.payload;
    const body = document.body;
    if (recordingDetected) {
      body.classList.add("hidden");
      setContent(`<p class="placeholder">检测到录屏，内容已隐藏</p>`);
    } else {
      body.classList.remove("hidden");
      if (lastAnswer) renderAnswer(lastAnswer);
    }
  });

  // Drag the window by the top bar.
  document.getElementById("bar")?.addEventListener("mousedown", async (event) => {
    if ((event.target as Element).closest("button")) return;
    try { await win.startDragging(); } catch { /* ignore */ }
  });

  pinButton.addEventListener("click", async (event) => {
    event.stopPropagation();
    pinned = !pinned;
    await win.setAlwaysOnTop(pinned);
    localStorage.setItem("qa-overlay-pinned", String(pinned));
    renderPin();
  });

  document.getElementById("hide")?.addEventListener("click", async (event) => {
    event.stopPropagation();
    await win.hide();
  });
}

main();
