# QA Snapshot

本地截图、云端视觉问答、手机远程控制的跨平台应用。

## 架构

```text
手机云端网页
    │  HTTPS / WebSocket
    ▼
qa-api 云服务 ───── LLM Provider
    │  设备命令 / SSE 流式回答
    ▼
Tauri 桌面客户端 ── 本地截图 + Overlay
```

- 截图始终在用户电脑本地执行。
- 手机网页部署在云端，不能直接访问电脑屏幕。
- 桌面客户端主动连接云端，接收已鉴权的截图命令。
- 截图通过 HTTPS 上传，模型回答同时流向手机网页和桌面 Overlay。
- 云端用 SQLite 保存 Session、提示词、截图和回答历史，电脑与手机浏览器共享同一份记录。
- 网页使用用户名和密码登录；密码以 Argon2id 哈希保存，登录状态使用服务端 Session Cookie。
- 普通用户只能访问分配给自己的设备；管理员可以管理用户、重置任意用户密码、分配设备并查看全部历史。
- Anthropic/OpenAI 等模型密钥只配置在云服务。

界面职责：

- `QA Control` 是桌面后台控制台，只负责云端配置、连接状态、手动截图和 Overlay 开关，不显示回答正文。
- `QA Overlay` 是桌面小窗，按模型增量实时显示回答，并支持 Markdown 标题、列表、代码块、表格和链接。
- `cloud-web` 同时服务电脑浏览器和手机浏览器。它采用响应式布局，以“本地截图气泡 + AI 回答气泡”显示实时问答和历史记录。
- Markdown 在浏览器中渲染前会经过 HTML 清理，避免模型输出直接注入不安全标签。

## 项目目录

```text
qaplugin/
├── frontend/                    # 桌面 Control / Overlay
├── src-tauri/                   # Tauri 桌面客户端
│   └── src/
│       ├── platform/
│       │   ├── macos.rs         # macOS 截图、Overlay 保护
│       │   ├── windows.rs       # Windows 截图、Overlay 保护
│       │   └── linux.rs
│       ├── cloud.rs             # 云端 HTTP/SSE/设备 WebSocket 客户端
│       ├── screenshot.rs        # 本地截图编排
│       └── hub.rs               # 桌面窗口事件总线
├── apps/cloud-web/              # 部署到云端的手机网页
├── services/qa-api/             # Rust/Axum 云端服务和 LLM 调用
├── crates/protocol/             # 桌面端与云端共享协议
├── deploy/                      # Docker + Caddy HTTPS 部署
├── .env.desktop.example
├── .env.cloud.example
└── DEPLOY.md
```

## 本地开发

### 1. 安装依赖

```bash
npm install
npm --prefix apps/cloud-web install
```

安装 Rust stable，以及当前平台所需的 Tauri 系统依赖。

### 2. 配置云端进程

```bash
cp .env.cloud.example .env.cloud
```

至少设置：

```dotenv
QA_BIND_ADDR=127.0.0.1:8080
QA_ADMIN_USERNAME=admin
QA_ADMIN_PASSWORD=your-long-admin-password
QA_COOKIE_SECURE=false
QA_DEVICE_TOKENS=desktop-1=your-device-token
LLM_PROVIDER=anthropic
LLM_API_KEY=your-provider-key
```

启动本地云服务：

```bash
./start-cloud.sh
```

服务默认监听 `http://127.0.0.1:8080`，网页也由该服务提供。

### 3. 配置桌面进程

直接启动桌面端：

```bash
./start.sh
```

在 Control 窗口填写云端 API 地址、网页地址、设备 ID 和设备 Token，点击“保存并重新连接”。配置会保存在当前用户的应用配置目录中，后续启动会自动加载。

开发和自动化环境也可以用环境变量覆盖已保存配置：

```bash
cp .env.desktop.example .env.desktop
```

确保设备 ID 和 Token 与云端匹配：

```dotenv
QA_CLOUD_URL=http://127.0.0.1:8080
QA_WEB_URL=http://127.0.0.1:8080
QA_DEVICE_ID=desktop-1
QA_DEVICE_TOKEN=your-device-token
```

使用阿里云百炼 Qwen 视觉模型时，云端模型配置可写为：

```dotenv
LLM_PROVIDER=qwen
LLM_API_KEY=your-dashscope-api-key
LLM_MODEL=qwen3-vl-plus
LLM_BASE_URL=https://dashscope.aliyuncs.com/compatible-mode
```

也可以填写百炼控制台提供的工作空间专属 `/compatible-mode/v1` 地址。桌面截图会在云端调用模型前转换为 Base64 Data URL；超过限制的大截图会自动缩放并压缩为 JPEG。

`start-cloud.sh` 在存在 `.env.cloud` 时只读取该文件，不会混入旧 `.env` 的 DeepSeek/OpenAI 地址；切换模型供应商时请把完整的 `LLM_*` 配置放在 `.env.cloud`。

### 4. 测试

浏览器打开：

```text
http://127.0.0.1:8080/?device_id=desktop-1
```

使用 `QA_ADMIN_USERNAME` 和首次配置的 `QA_ADMIN_PASSWORD` 登录。设备在线后可从网页触发电脑截图，也可以使用：

```text
Ctrl + Shift + Space
```

云端网页是一套响应式页面：宽屏显示左侧历史 Session，手机窄屏显示可横向浏览的会话栏。每个 Session 可以配置名称和提示词，并保存多次截图问答记录。每次记录用截图气泡和 Markdown AI 气泡呈现；回答通过 WebSocket 实时追加。默认数据库位于 `data/qa-snapshot.db`。

管理员点击右上角“用户管理”可以创建普通用户、重置任意账户密码和修改设备归属。重置密码会立即撤销该用户所有旧登录；管理员在设备选择器中始终可以选择全部设备，因此可以查看所有用户的 Session、截图和回答。首次升级旧数据库时，已有设备与历史会自动归属首个 `admin`。旧版 `QA_WEB_TOKEN` 仅兼容用作首次创建 admin 的密码来源，网页不再使用 Token 登录。

## 质量检查

```bash
npm run check
```

它会检查桌面 TypeScript、构建桌面前端，并检查整个 Rust workspace。云端网页可单独检查：

```bash
npm --prefix apps/cloud-web run build
```

## 桌面打包

macOS 构建必须在 macOS 上进行：

```bash
npm run tauri:build -- --bundles app,dmg
```

Windows 推荐在 Windows 上构建：

```powershell
npm install
npm run tauri:build -- --bundles nsis
```

每位用户安装后在 Control 设置页填写自己的 `QA_DEVICE_ID`、`QA_DEVICE_TOKEN` 和云服务地址即可。当前配置文件中的设备 Token 是明文保存；正式发布前建议接入 macOS Keychain 和 Windows Credential Manager。

## 云端部署

完整步骤见 [DEPLOY.md](./DEPLOY.md)。快速入口：

```bash
cp .env.cloud.example .env.cloud
docker compose --env-file .env.cloud -f deploy/docker-compose.yml up -d --build
```

公网部署必须配置域名和 HTTPS。

## 当前限制

- Linux 截图和反截屏尚未实现。
- Windows 截图暂时使用 PowerShell `System.Drawing`，后续可替换为 Windows Graphics Capture。
- Session、提示词、截图和答案保存在 SQLite；实时设备连接状态仍保存在内存中。
- 当前按单个云服务实例设计；多实例需要 Redis 消息中继，并将 SQLite 替换为共享数据库/对象存储。
- 云端设备 Token 暂由环境变量配置，尚未提供数据库、后台管理和自动注册流程。
- 桌面设置已支持持久化，但设备 Token 暂未接入系统凭据存储。
