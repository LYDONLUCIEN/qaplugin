// Overlay logic: listens to hub events + the macOS "screen-captured"
// signal from anti-capture polling. When recording is detected the
// content gets blanked (placeholder shown); when it stops the latest
// answer re-appears.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { renderMarkdown } from "./markdown";

interface HubEvent {
  type: "Capturing" | "Uploading" | "Streaming" | "Done" | "Error";
  delta?: string;
  answer?: string;
  message?: string;
  model_name?: string;
  ttft_ms?: number | null;
  total_ms?: number | null;
}

const win = getCurrentWebviewWindow();
const content = document.getElementById("content")!;
const answerMeta = document.getElementById("answer-meta") as HTMLElement;
const jumpLatestButton = document.getElementById("jump-latest") as HTMLButtonElement;
const historyButton = document.getElementById("show-history") as HTMLButtonElement;
const BOTTOM_THRESHOLD = 36;
let followLatest = true;

function isNearBottom() {
  return content.scrollHeight - content.scrollTop - content.clientHeight <= BOTTOM_THRESHOLD;
}

function updateFollowControl() {
  jumpLatestButton.hidden = followLatest;
}

function resumeFollowing() {
  followLatest = true;
  content.scrollTop = content.scrollHeight;
  updateFollowControl();
}

content.addEventListener("scroll", () => {
  followLatest = isNearBottom();
  updateFollowControl();
}, { passive: true });

jumpLatestButton.addEventListener("click", (event) => {
  event.stopPropagation();
  resumeFollowing();
});

historyButton.addEventListener("click", (event) => {
  event.stopPropagation();
  showingHistory = !showingHistory;
  historyButton.textContent = showingHistory ? "当前回答" : "本次历史";
  if (showingHistory) renderHistory();
  else renderAnswer(lastAnswer);
});

function setState(text: string, cls: "idle" | "busy" | "err") {
  const dot = document.querySelector(".dot") as HTMLElement;
  dot.classList.remove("busy", "err");
  if (cls === "busy") dot.classList.add("busy");
  if (cls === "err") dot.classList.add("err");
  document.getElementById("state")!.textContent = text;
}

function setContent(html: string) {
  followLatest = true;
  content.classList.remove("markdown-body");
  content.innerHTML = html;
  content.scrollTop = 0;
  updateFollowControl();
}

let lastAnswer = "";
let showingHistory = false;
const history: Array<{ answer: string; modelName: string; ttftMs: number | null; totalMs: number | null }> = [];

function timingLabel(modelName: string, ttftMs: number | null, totalMs: number | null) {
  const parts = [modelName].filter(Boolean);
  if (ttftMs != null) parts.push(`TTFT ${(ttftMs / 1000).toFixed(2)}s`);
  if (totalMs != null) parts.push(`完成 ${(totalMs / 1000).toFixed(2)}s`);
  return parts.join(" · ");
}

function renderHistory() {
  followLatest = false;
  content.replaceChildren();
  content.classList.remove("markdown-body");
  if (!history.length) {
    content.innerHTML = '<p class="placeholder">本次运行还没有完成的回答。</p>';
    return;
  }
  for (const item of [...history].reverse()) {
    const entry = document.createElement("section");
    entry.className = "overlay-history-entry";
    const meta = document.createElement("div");
    meta.className = "overlay-history-meta";
    meta.textContent = timingLabel(item.modelName, item.ttftMs, item.totalMs) || "AI 回复";
    const answer = document.createElement("div");
    answer.className = "markdown-body";
    renderMarkdown(answer, item.answer);
    entry.append(meta, answer);
    content.append(entry);
  }
  content.scrollTop = 0;
  updateFollowControl();
}

function renderAnswer(text: string) {
  lastAnswer = text;
  if (recordingDetected || showingHistory) return; // will re-render when capture stops
  const previousScrollTop = content.scrollTop;
  const shouldFollow = followLatest || isNearBottom();
  content.classList.add("markdown-body");
  renderMarkdown(content, text);
  if (shouldFollow) {
    content.scrollTop = content.scrollHeight;
    followLatest = true;
  } else {
    content.scrollTop = previousScrollTop;
    followLatest = false;
  }
  updateFollowControl();
}

function escapeHtml(s: string) {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

let recordingDetected = false;

async function main() {
  try {
    const version = await invoke<string>("get_app_version");
    document.getElementById("app-version")!.textContent = `v${version}`;
  } catch (error) {
    console.warn("could not read app version", error);
  }

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
        showingHistory = false;
        historyButton.textContent = "本次历史";
        lastAnswer = "";
        answerMeta.hidden = true;
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
        lastAnswer = ev.answer ?? lastAnswer;
        const modelName = ev.model_name ?? "";
        const ttftMs = ev.ttft_ms ?? null;
        const totalMs = ev.total_ms ?? null;
        history.push({ answer: lastAnswer, modelName, ttftMs, totalMs });
        const metrics = timingLabel(modelName, ttftMs, totalMs);
        setState("done", "idle");
        answerMeta.textContent = metrics;
        answerMeta.hidden = !metrics;
        renderAnswer(lastAnswer);
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
