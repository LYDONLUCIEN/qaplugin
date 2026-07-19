# QA Snapshot 云端部署

## 运行架构

- 云服务器运行 `qa-api`、云端手机网页和 Caddy。
- 每台电脑运行 Tauri 桌面客户端，并主动连接云端 WebSocket。
- 手机只访问云端网页。点击“截图”后，云端把命令转发给指定桌面客户端。
- 截图在电脑本地生成，随后通过 HTTPS 上传云端。
- LLM Key 只存在于云服务器。

Session、提示词、截图和回答历史保存在 SQLite。Docker Compose 使用 `qa_data` 数据卷挂载 `/app/data`，更新或重启容器不会清空历史；实时设备在线状态仍保存在内存中。

## 1. 准备服务器

建议使用带公网 IP 的 Linux 服务器，并准备域名，例如 `qa.example.com`。

安装 Docker Engine 和 Docker Compose Plugin，然后将域名的 A/AAAA 记录指向服务器。防火墙只需开放：

```text
TCP 22   SSH
TCP 80   HTTP/证书签发
TCP 443  HTTPS/WebSocket
UDP 443  HTTP/3（可选）
```

## 2. 配置云端环境

在项目根目录执行：

```bash
cp .env.cloud.example .env.cloud
openssl rand -hex 32
openssl rand -hex 32
```

把两个随机值分别设置为初始管理员密码和设备 Token：

```dotenv
QA_DOMAIN=qa.example.com
QA_ADMIN_USERNAME=admin
QA_ADMIN_PASSWORD=<第一个随机值，至少 12 个字符>
QA_AUTH_SESSION_HOURS=168
QA_COOKIE_SECURE=true
QA_DEVICE_TOKENS=desktop-1=<第二个随机值>
QA_DB_PATH=/app/data/qa-snapshot.db

LLM_PROVIDER=anthropic
LLM_API_KEY=<云端模型密钥>
LLM_MODEL=claude-sonnet-4-6
```

如使用阿里云百炼 Qwen 视觉模型：

```dotenv
LLM_PROVIDER=qwen
LLM_API_KEY=<百炼 API Key>
LLM_MODEL=qwen3-vl-plus
LLM_BASE_URL=https://dashscope.aliyuncs.com/compatible-mode
```

也可以使用百炼控制台给出的工作空间专属 `/compatible-mode/v1` 地址。服务兼容带或不带 `/v1` 的 Base URL，并会把过大的 PNG 截图自动压缩后以 Base64 Data URL 发送。

模型供应商相关的 `LLM_*` 变量应完整写在 `.env.cloud`；存在该文件时，`start-cloud.sh` 不再混入旧 `.env` 配置。

多台电脑使用逗号分隔：

```dotenv
QA_DEVICE_TOKENS=alice-mac=<token1>,bob-win=<token2>,office-pc=<token3>
```

设备 ID 只能包含英文字母、数字、`-` 和 `_`。

## 3. 启动云服务

```bash
docker compose --env-file .env.cloud -f deploy/docker-compose.yml up -d --build
docker compose --env-file .env.cloud -f deploy/docker-compose.yml ps
docker compose --env-file .env.cloud -f deploy/docker-compose.yml logs -f qa-api
```

Caddy 会自动申请和续期 HTTPS 证书。验证：

```bash
curl https://qa.example.com/healthz
```

预期结果：

```json
{"ok":true,"version":"0.1.0"}
```

浏览器访问 `https://qa.example.com/?device_id=desktop-1`，使用 `QA_ADMIN_USERNAME` 和 `QA_ADMIN_PASSWORD` 登录。首次启动时服务会创建管理员，并把已有设备和历史归到该管理员。之后可以在右上角“用户管理”中创建用户、重置密码和分配设备。

`QA_ADMIN_PASSWORD` 只在数据库尚无管理员时用于初始化，密码只以 Argon2id 哈希写入数据库。管理员在页面修改自己的密码后可以从环境文件移除初始明文密码；后续服务启动会继续使用数据库账户。管理员重置任何用户密码时，该用户已有登录都会立即失效。

## 4. 配置桌面客户端

安装并打开桌面客户端后，在 Control 窗口填写：

```text
API 地址：https://qa.example.com
网页地址：https://qa.example.com
设备 ID：desktop-1
设备 Token：与 QA_DEVICE_TOKENS 中 desktop-1 对应的 Token
```

点击“保存并重新连接”。这套配置保存在当前用户的应用配置目录中，桌面程序以后会自动连接云端。

源码开发或自动化部署时，也可以通过 `.env.desktop` 覆盖设置页配置：

```bash
cp .env.desktop.example .env.desktop
```

内容示例：

```dotenv
QA_CLOUD_URL=https://qa.example.com
QA_WEB_URL=https://qa.example.com
QA_DEVICE_ID=desktop-1
QA_DEVICE_TOKEN=<与 QA_DEVICE_TOKENS 中 desktop-1 对应的 Token>
```

源码开发运行：

```bash
./start.sh
```

桌面端连接成功后，云端网页会显示设备在线。手机点击触发后，命令流程为：

```text
手机网页 → 云端 WebSocket → 桌面截图 → HTTPS 上传 → 云端 LLM → 手机网页 + 桌面 Overlay
```

云端 LLM 请求使用 SSE 流式响应，每个回答增量会立即广播到电脑和手机网页，并同步送到桌面 Overlay。网页和 Overlay 会把累计内容作为经过清理的 Markdown 富文本重新渲染；QA Control 只显示运行状态，不承载回答正文。

## 5. 本地联调

不使用域名和 Docker 时，可在开发电脑上启动云服务：

```bash
cp .env.cloud.example .env.cloud
# 使用 QA_BIND_ADDR=127.0.0.1:8080，并设置 QA_COOKIE_SECURE=false
npm --prefix apps/cloud-web install
./start-cloud.sh
```

另开终端运行桌面端：

```bash
cp .env.desktop.example .env.desktop
./start.sh
```

本机浏览器访问 `http://127.0.0.1:8080/?device_id=desktop-1`。

如果要让同一 Wi-Fi 的手机参与本地联调，把桌面配置中的 URL 改为电脑局域网 IP，例如 `http://192.168.1.10:8080`，并让云服务监听 `0.0.0.0:8080`。

## 6. 更新服务

```bash
docker compose --env-file .env.cloud -f deploy/docker-compose.yml up -d --build
docker image prune
```

`docker image prune` 只清理未使用镜像；执行前可先用 `docker image ls` 检查。

## 安全注意事项

- 不要把 `.env.cloud`、`.env.desktop` 或任何 Token 提交到仓库。
- 管理员初始登录完成并修改密码后，从 `.env.cloud` 移除 `QA_ADMIN_PASSWORD`；数据库中只保留 Argon2id 密码哈希。
- 公网部署保持 `QA_COOKIE_SECURE=true`，登录 Cookie 使用 `HttpOnly` 和 `SameSite=Strict`。
- 不要把 LLM Key 放进桌面安装包。
- 每台设备使用独立 Token；设备丢失时只撤销对应 Token。
- 桌面端当前会把设备 Token 明文保存在当前用户的应用配置目录；正式分发前建议接入 macOS Keychain / Windows Credential Manager。
- 公网部署必须使用 HTTPS，不能使用明文 HTTP 上传截图。
- 云服务器、反向代理和应用日志不要记录请求正文或 Authorization Header。
- 当前 SQLite + 内存连接状态适合单实例部署。以后扩容多个 API 实例时，需要使用 Redis 做设备连接/事件中继，并使用共享数据库和对象存储。
