import "./styles.css";
import { renderMarkdown } from "./markdown";

const DEFAULT_PROMPT =
  "请分析这张桌面截图：识别主要界面和可见文字，回答截图中的问题或任务，并给出简洁、可执行的说明。";

interface Account {
  id: string;
  username: string;
  is_admin: boolean;
}

interface DeviceSummary {
  device_id: string;
  owner_user_id: string;
  owner_username: string;
}

interface AuthView {
  user: Account;
  devices: DeviceSummary[];
}

interface AdminUser {
  id: string;
  username: string;
  is_admin: boolean;
  device_count: number;
  session_count: number;
  turn_count: number;
  created_at: number;
}

interface AdminView {
  users: AdminUser[];
  devices: DeviceSummary[];
}

interface SessionSummary {
  id: string;
  device_id: string;
  title: string;
  prompt: string;
  created_at: number;
  updated_at: number;
  turn_count: number;
}

interface TurnRecord {
  id: string;
  session_id: string;
  prompt: string;
  screenshot_b64: string;
  screenshot_mime: string;
  answer: string;
  status: string;
  created_at: number;
  updated_at: number;
}

type QaEvent =
  | { type: "Capturing" }
  | { type: "Uploading" }
  | { type: "Screenshot"; image_b64: string; mime_type: string }
  | { type: "Streaming"; delta: string }
  | { type: "Done"; answer: string }
  | { type: "Error"; message: string }
  | { type: "DeviceStatus"; connected: boolean }
  | {
      type: "Snapshot";
      connected: boolean;
      screenshot_b64?: string | null;
      screenshot_mime?: string | null;
      answer: string;
      status: string;
    }
  | {
      type: "SessionList";
      sessions: SessionSummary[];
      active_session_id?: string | null;
    }
  | { type: "SessionDetail"; session: SessionSummary; turns: TurnRecord[] };

const apiBase = (import.meta.env.VITE_QA_API_URL || location.origin).replace(/\/$/, "");
const $ = <T extends Element>(selector: string) => document.querySelector<T>(selector)!;

const loginCardEl = $("#login-card") as HTMLElement;
const appShellEl = $("#app-shell") as HTMLElement;
const accountActionsEl = $("#account-actions") as HTMLElement;
const usernameInput = $("#username") as HTMLInputElement;
const passwordInput = $("#password") as HTMLInputElement;
const loginButton = $("#login") as HTMLButtonElement;
const loginStatusEl = $("#login-status") as HTMLElement;
const currentUserEl = $("#current-user") as HTMLElement;
const logoutButton = $("#logout") as HTMLButtonElement;
const adminToggleButton = $("#admin-toggle") as HTMLButtonElement;
const adminCloseButton = $("#admin-close") as HTMLButtonElement;
const adminPanelEl = $("#admin-panel") as HTMLElement;
const adminUsersEl = $("#admin-users") as HTMLElement;
const adminDevicesEl = $("#admin-devices") as HTMLElement;
const adminStatusEl = $("#admin-status") as HTMLElement;
const newUsernameInput = $("#new-username") as HTMLInputElement;
const newPasswordInput = $("#new-password") as HTMLInputElement;
const createUserButton = $("#create-user") as HTMLButtonElement;
const deviceSelect = $("#device-id") as HTMLSelectElement;
const connectButton = $("#connect") as HTMLButtonElement;
const captureButton = $("#capture") as HTMLButtonElement;
const newSessionButton = $("#new-session") as HTMLButtonElement;
const saveSessionButton = $("#save-session") as HTMLButtonElement;
const titleInput = $("#session-title") as HTMLInputElement;
const promptInput = $("#prompt") as HTMLTextAreaElement;
const connectionEl = $("#connection") as HTMLElement;
const deviceStateEl = $("#device-state") as HTMLElement;
const statusEl = $("#status") as HTMLElement;
const answerEl = $("#live-answer") as HTMLElement;
const answerStateEl = $("#answer-state") as HTMLElement;
const livePromptEl = $("#live-prompt") as HTMLElement;
const screenshotEl = $("#live-screenshot") as HTMLImageElement;
const previewEmptyEl = $("#live-preview-empty") as HTMLElement;
const liveCardEl = $("#live-card") as HTMLElement;
const sessionListEl = $("#session-list") as HTMLElement;
const turnListEl = $("#turn-list") as HTMLElement;
const turnCountEl = $("#turn-count") as HTMLElement;

let socket: WebSocket | undefined;
let liveAnswer = "";
let currentAuth: AuthView | undefined;
let socketReady = false;
let deviceOnline = false;
let retryTimer: number | undefined;
let sessions: SessionSummary[] = [];
let activeSessionId: string | undefined;

promptInput.value = DEFAULT_PROMPT;
usernameInput.value = localStorage.getItem("qa-username") || "admin";

async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers);
  if (init.body) headers.set("content-type", "application/json");
  const response = await fetch(`${apiBase}${path}`, {
    ...init,
    headers,
    credentials: "include",
  });
  const body = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(String((body as { error?: string }).error || `请求失败 (${response.status})`));
  }
  return body as T;
}

function setConnection(text: string, online: boolean) {
  connectionEl.textContent = text;
  connectionEl.classList.toggle("online", online);
  connectionEl.classList.toggle("offline", !online);
}

function selectedDeviceId() {
  return deviceSelect.value;
}

function updateControls() {
  const hasSession = Boolean(activeSessionId);
  const hasDevice = Boolean(selectedDeviceId());
  connectButton.disabled = !currentAuth || !hasDevice;
  captureButton.disabled = !socketReady || !deviceOnline || !hasSession;
  newSessionButton.disabled = !socketReady;
  saveSessionButton.disabled = !socketReady || !hasSession;
  titleInput.disabled = !socketReady || !hasSession;
  promptInput.disabled = !socketReady || !hasSession;
  if (!currentAuth) {
    deviceStateEl.textContent = "请先登录";
  } else if (!hasDevice) {
    deviceStateEl.textContent = "当前账户还没有分配设备，请联系管理员";
  } else if (!socketReady) {
    deviceStateEl.textContent = `正在连接设备 ${selectedDeviceId()}…`;
  } else {
    deviceStateEl.textContent = deviceOnline
      ? `设备 ${selectedDeviceId()} 在线`
      : `设备 ${selectedDeviceId()} 离线，历史仍可浏览`;
  }
}

function send(message: object) {
  if (socket?.readyState === WebSocket.OPEN && socketReady) {
    socket.send(JSON.stringify(message));
  }
}

function showLiveScreenshot(base64?: string | null, mime = "image/png") {
  if (!base64) return;
  liveCardEl.hidden = false;
  screenshotEl.src = `data:${mime};base64,${base64}`;
  screenshotEl.hidden = false;
  previewEmptyEl.hidden = true;
}

function renderDevices(devices: DeviceSummary[]) {
  const preferred =
    new URLSearchParams(location.search).get("device_id") ||
    localStorage.getItem("qa-device-id") ||
    "";
  deviceSelect.replaceChildren();
  for (const device of devices) {
    const option = document.createElement("option");
    option.value = device.device_id;
    option.textContent = currentAuth?.user.is_admin
      ? `${device.device_id} · ${device.owner_username}`
      : device.device_id;
    deviceSelect.append(option);
  }
  if (devices.some((device) => device.device_id === preferred)) {
    deviceSelect.value = preferred;
  }
}

function renderSessions() {
  sessionListEl.replaceChildren();
  if (!sessions.length) {
    const empty = document.createElement("p");
    empty.className = "empty-copy";
    empty.textContent = "还没有历史会话";
    sessionListEl.append(empty);
    return;
  }
  for (const session of sessions) {
    const button = document.createElement("button");
    button.className = "session-item";
    button.classList.toggle("active", session.id === activeSessionId);
    button.type = "button";
    const title = document.createElement("strong");
    title.textContent = session.title || "未命名会话";
    const meta = document.createElement("span");
    meta.textContent = `${session.turn_count} 条 · ${formatTime(session.updated_at)}`;
    button.append(title, meta);
    button.addEventListener("click", () => {
      activeSessionId = session.id;
      renderSessions();
      updateControls();
      send({ type: "SelectSession", session_id: session.id });
    });
    sessionListEl.append(button);
  }
}

function renderSession(session: SessionSummary, turns: TurnRecord[]) {
  activeSessionId = session.id;
  titleInput.value = session.title;
  promptInput.value = session.prompt || DEFAULT_PROMPT;
  const index = sessions.findIndex((item) => item.id === session.id);
  if (index >= 0) sessions[index] = session;
  else sessions.unshift(session);
  renderSessions();
  renderTurns(turns);
  updateControls();
}

function renderTurns(turns: TurnRecord[]) {
  turnListEl.replaceChildren();
  turnCountEl.textContent = `${turns.length} 条`;
  if (!turns.length) {
    const empty = document.createElement("div");
    empty.className = "card empty-history";
    empty.textContent = "这个会话还没有截图，配置提示词后即可开始。";
    turnListEl.append(empty);
    return;
  }

  for (const turn of turns) {
    const article = document.createElement("article");
    article.className = "card turn-card message-thread";
    const header = document.createElement("div");
    header.className = "turn-header";
    const time = document.createElement("time");
    time.textContent = formatDateTime(turn.created_at);
    const state = document.createElement("span");
    state.className = turn.status.startsWith("error") ? "turn-state error" : "turn-state";
    state.textContent = statusLabel(turn.status);
    header.append(time, state);

    const userRow = document.createElement("div");
    userRow.className = "message-row user-message";
    const userAvatar = document.createElement("div");
    userAvatar.className = "message-avatar";
    userAvatar.textContent = "你";
    const userBubble = document.createElement("div");
    userBubble.className = "message-bubble image-bubble";
    const userLabel = document.createElement("div");
    userLabel.className = "message-label";
    userLabel.textContent = "本地电脑截图";
    const prompt = document.createElement("p");
    prompt.className = "message-prompt";
    prompt.textContent = turn.prompt;
    const image = document.createElement("img");
    image.loading = "lazy";
    image.alt = "本地电脑历史截图";
    image.src = `data:${turn.screenshot_mime};base64,${turn.screenshot_b64}`;
    userBubble.append(userLabel, prompt, image);
    userRow.append(userAvatar, userBubble);

    const assistantRow = document.createElement("div");
    assistantRow.className = "message-row assistant-message";
    const aiAvatar = document.createElement("div");
    aiAvatar.className = "message-avatar ai";
    aiAvatar.textContent = "AI";
    const answerBubble = document.createElement("div");
    answerBubble.className = "message-bubble answer-bubble";
    const answerLabel = document.createElement("div");
    answerLabel.className = "message-label";
    answerLabel.textContent = "AI 回复";
    const answer = document.createElement("div");
    answer.className = "markdown-body";
    const answerText =
      turn.answer || (turn.status.startsWith("error") ? turn.status : "等待回答…");
    if (turn.status.startsWith("error") && !turn.answer) answer.textContent = answerText;
    else renderMarkdown(answer, answerText);
    answerBubble.append(answerLabel, answer);
    assistantRow.append(aiAvatar, answerBubble);
    article.append(header, userRow, assistantRow);
    turnListEl.append(article);
  }
}

function handleEvent(event: QaEvent) {
  switch (event.type) {
    case "Snapshot":
      socketReady = true;
      deviceOnline = event.connected;
      liveAnswer = event.answer || "";
      renderMarkdown(answerEl, liveAnswer || "尚无实时回答。");
      statusEl.textContent = event.status || "idle";
      if (event.screenshot_b64) {
        showLiveScreenshot(event.screenshot_b64, event.screenshot_mime || "image/png");
      }
      setConnection("设备已连接", true);
      updateControls();
      break;
    case "SessionList":
      sessions = event.sessions;
      if (event.active_session_id) activeSessionId = event.active_session_id;
      renderSessions();
      updateControls();
      break;
    case "SessionDetail":
      if (!activeSessionId || activeSessionId === event.session.id) {
        renderSession(event.session, event.turns);
      } else {
        const index = sessions.findIndex((item) => item.id === event.session.id);
        if (index >= 0) sessions[index] = event.session;
        renderSessions();
      }
      break;
    case "DeviceStatus":
      deviceOnline = event.connected;
      updateControls();
      break;
    case "Capturing":
      liveCardEl.hidden = false;
      screenshotEl.hidden = true;
      previewEmptyEl.hidden = false;
      previewEmptyEl.textContent = "桌面端正在截图…";
      livePromptEl.textContent = promptInput.value.trim() || DEFAULT_PROMPT;
      liveAnswer = "";
      answerEl.textContent = "等待截图上传…";
      statusEl.textContent = "桌面端正在截图…";
      answerStateEl.textContent = "截图中";
      break;
    case "Screenshot":
      showLiveScreenshot(event.image_b64, event.mime_type);
      break;
    case "Uploading":
      liveCardEl.hidden = false;
      liveAnswer = "";
      answerEl.textContent = "模型正在分析截图…";
      statusEl.textContent = "截图已保存，正在分析…";
      answerStateEl.textContent = "分析中";
      break;
    case "Streaming":
      liveAnswer += event.delta;
      renderMarkdown(answerEl, liveAnswer);
      statusEl.textContent = "正在生成回答…";
      answerStateEl.textContent = "流式输出";
      break;
    case "Done":
      liveAnswer = event.answer || liveAnswer;
      renderMarkdown(answerEl, liveAnswer || "回答为空。");
      statusEl.textContent = "完成，已写入会话历史";
      answerStateEl.textContent = "已完成";
      break;
    case "Error":
      liveCardEl.hidden = false;
      statusEl.textContent = `错误：${event.message}`;
      answerEl.textContent = event.message;
      answerStateEl.textContent = "出错";
      break;
  }
}

function websocketUrl(deviceId: string) {
  const url = new URL(apiBase);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  url.pathname = "/v1/web/ws";
  url.search = new URLSearchParams({ device_id: deviceId }).toString();
  return url.toString();
}

function clearConversation() {
  socketReady = false;
  deviceOnline = false;
  activeSessionId = undefined;
  sessions = [];
  renderSessions();
  turnCountEl.textContent = "0 条";
  turnListEl.innerHTML = '<div class="card empty-history">正在加载设备历史…</div>';
  liveCardEl.hidden = true;
  titleInput.value = "";
  promptInput.value = DEFAULT_PROMPT;
}

function connectDevice() {
  const deviceId = selectedDeviceId();
  if (!currentAuth || !deviceId) return;
  window.clearTimeout(retryTimer);
  socket?.close();
  clearConversation();
  updateControls();
  setConnection("连接中…", false);
  localStorage.setItem("qa-device-id", deviceId);

  const nextSocket = new WebSocket(websocketUrl(deviceId));
  socket = nextSocket;
  nextSocket.onmessage = (message) => {
    if (socket !== nextSocket) return;
    try {
      handleEvent(JSON.parse(String(message.data)) as QaEvent);
    } catch {
      statusEl.textContent = "收到无法解析的云端消息。";
    }
  };
  nextSocket.onerror = () => {
    if (socket === nextSocket) setConnection("连接错误", false);
  };
  nextSocket.onclose = () => {
    if (socket !== nextSocket) return;
    const shouldRetry = Boolean(currentAuth && socketReady);
    socket = undefined;
    socketReady = false;
    deviceOnline = false;
    setConnection("已断开", false);
    updateControls();
    if (shouldRetry) retryTimer = window.setTimeout(connectDevice, 2000);
  };
}

function enterApp(view: AuthView) {
  currentAuth = view;
  loginCardEl.hidden = true;
  appShellEl.hidden = false;
  accountActionsEl.hidden = false;
  currentUserEl.textContent = view.user.is_admin
    ? `${view.user.username} · 管理员`
    : view.user.username;
  adminToggleButton.hidden = !view.user.is_admin;
  passwordInput.value = "";
  renderDevices(view.devices);
  setConnection("已登录", true);
  updateControls();
  if (selectedDeviceId()) connectDevice();
}

function showLogin(message = "") {
  window.clearTimeout(retryTimer);
  socket?.close();
  socket = undefined;
  currentAuth = undefined;
  socketReady = false;
  loginCardEl.hidden = false;
  appShellEl.hidden = true;
  accountActionsEl.hidden = true;
  adminPanelEl.hidden = true;
  loginStatusEl.textContent = message;
  setConnection("未登录", false);
}

async function performLogin() {
  const username = usernameInput.value.trim();
  const password = passwordInput.value;
  if (!username || !password) {
    loginStatusEl.textContent = "请输入用户名和密码。";
    return;
  }
  loginButton.disabled = true;
  loginStatusEl.textContent = "正在登录…";
  try {
    const view = await api<AuthView>("/v1/auth/login", {
      method: "POST",
      body: JSON.stringify({ username, password }),
    });
    localStorage.setItem("qa-username", view.user.username);
    loginStatusEl.textContent = "";
    enterApp(view);
  } catch (error) {
    loginStatusEl.textContent = String(error instanceof Error ? error.message : error);
  } finally {
    loginButton.disabled = false;
  }
}

async function loadAdmin() {
  adminStatusEl.textContent = "正在加载…";
  try {
    const view = await api<AdminView>("/v1/admin/users");
    renderAdmin(view);
    adminStatusEl.textContent = "";
  } catch (error) {
    adminStatusEl.textContent = String(error instanceof Error ? error.message : error);
  }
}

function renderAdmin(view: AdminView) {
  adminUsersEl.replaceChildren();
  for (const user of view.users) {
    const row = document.createElement("article");
    row.className = "admin-row";
    const summary = document.createElement("div");
    const name = document.createElement("strong");
    name.textContent = user.is_admin ? `${user.username} · 管理员` : user.username;
    const stats = document.createElement("span");
    stats.textContent = `${user.device_count} 台设备 · ${user.session_count} 个会话 · ${user.turn_count} 条问答`;
    summary.append(name, stats);
    const actions = document.createElement("div");
    actions.className = "admin-row-actions";
    const password = document.createElement("input");
    password.type = "password";
    password.autocomplete = "new-password";
    password.placeholder = "设置新密码（至少 12 位）";
    const reset = document.createElement("button");
    reset.className = "compact";
    reset.textContent = "重置密码";
    reset.addEventListener("click", async () => {
      if (!password.value) return;
      reset.disabled = true;
      try {
        await api(`/v1/admin/users/${encodeURIComponent(user.id)}/password`, {
          method: "PUT",
          body: JSON.stringify({ password: password.value }),
        });
        password.value = "";
        if (user.id === currentAuth?.user.id) {
          showLogin("管理员密码已修改，请使用新密码重新登录。");
        } else {
          adminStatusEl.textContent = `${user.username} 的密码已重置，旧登录已失效。`;
        }
      } catch (error) {
        adminStatusEl.textContent = String(error instanceof Error ? error.message : error);
      } finally {
        reset.disabled = false;
      }
    });
    actions.append(password, reset);
    row.append(summary, actions);
    adminUsersEl.append(row);
  }

  adminDevicesEl.replaceChildren();
  for (const device of view.devices) {
    const row = document.createElement("article");
    row.className = "admin-row";
    const name = document.createElement("strong");
    name.textContent = device.device_id;
    const actions = document.createElement("div");
    actions.className = "admin-row-actions";
    const owner = document.createElement("select");
    for (const user of view.users) {
      const option = document.createElement("option");
      option.value = user.id;
      option.textContent = user.username;
      option.selected = user.id === device.owner_user_id;
      owner.append(option);
    }
    const assign = document.createElement("button");
    assign.className = "compact";
    assign.textContent = "保存归属";
    assign.addEventListener("click", async () => {
      assign.disabled = true;
      try {
        const updated = await api<AdminView>(
          `/v1/admin/devices/${encodeURIComponent(device.device_id)}/owner`,
          { method: "PUT", body: JSON.stringify({ user_id: owner.value }) },
        );
        renderAdmin(updated);
        const me = await api<AuthView>("/v1/auth/me");
        currentAuth = me;
        renderDevices(me.devices);
        adminStatusEl.textContent = `${device.device_id} 的归属已更新。`;
      } catch (error) {
        adminStatusEl.textContent = String(error instanceof Error ? error.message : error);
      } finally {
        assign.disabled = false;
      }
    });
    actions.append(owner, assign);
    row.append(name, actions);
    adminDevicesEl.append(row);
  }
}

loginButton.addEventListener("click", performLogin);
passwordInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") void performLogin();
});
logoutButton.addEventListener("click", async () => {
  try {
    await api("/v1/auth/logout", { method: "POST" });
  } finally {
    showLogin("已安全退出。");
  }
});
adminToggleButton.addEventListener("click", () => {
  adminPanelEl.hidden = false;
  void loadAdmin();
});
adminCloseButton.addEventListener("click", () => {
  adminPanelEl.hidden = true;
});
createUserButton.addEventListener("click", async () => {
  createUserButton.disabled = true;
  try {
    const view = await api<AdminView>("/v1/admin/users", {
      method: "POST",
      body: JSON.stringify({
        username: newUsernameInput.value.trim(),
        password: newPasswordInput.value,
      }),
    });
    newUsernameInput.value = "";
    newPasswordInput.value = "";
    renderAdmin(view);
    adminStatusEl.textContent = "用户已创建，可以继续为其分配设备。";
  } catch (error) {
    adminStatusEl.textContent = String(error instanceof Error ? error.message : error);
  } finally {
    createUserButton.disabled = false;
  }
});
connectButton.addEventListener("click", connectDevice);
deviceSelect.addEventListener("change", connectDevice);
newSessionButton.addEventListener("click", () => {
  send({ type: "CreateSession", title: "新会话", prompt: DEFAULT_PROMPT });
});
saveSessionButton.addEventListener("click", () => {
  if (!activeSessionId) return;
  send({
    type: "UpdateSession",
    session_id: activeSessionId,
    title: titleInput.value.trim() || "新会话",
    prompt: promptInput.value.trim() || DEFAULT_PROMPT,
  });
  statusEl.textContent = "会话设置已保存";
});
captureButton.addEventListener("click", () => {
  if (!activeSessionId || !socketReady || !deviceOnline) return;
  const prompt = promptInput.value.trim() || DEFAULT_PROMPT;
  send({
    type: "UpdateSession",
    session_id: activeSessionId,
    title: titleInput.value.trim() || "新会话",
    prompt,
  });
  send({ type: "Trigger", session_id: activeSessionId, question: prompt });
  statusEl.textContent = "触发命令已发送…";
});

function formatTime(timestamp: number) {
  return new Intl.DateTimeFormat("zh-CN", { hour: "2-digit", minute: "2-digit" }).format(
    new Date(timestamp * 1000),
  );
}

function formatDateTime(timestamp: number) {
  return new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(timestamp * 1000));
}

function statusLabel(status: string) {
  if (status === "done") return "已完成";
  if (status === "uploading") return "处理中";
  if (status.startsWith("error")) return "出错";
  return status;
}

async function bootstrap() {
  try {
    const view = await api<AuthView>("/v1/auth/me");
    enterApp(view);
  } catch {
    showLogin();
  }
}

void bootstrap();
