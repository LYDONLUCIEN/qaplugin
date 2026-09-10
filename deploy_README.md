可以。推荐按这个顺序进行：

   环境        主要职责
  ━━━━━━━━━━  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
   Mac         整理、测试、提交代码；也可编译 macOS 客户端
  ──────────  ────────────────────────────────────────────────────
   云服务器    运行 qa-api、网页、数据库、Caddy HTTPS、调用大模型
  ──────────  ────────────────────────────────────────────────────
   Windows     编译并安装 Windows 桌面截图客户端

  生产环境的 Docker 只需要安装在云服务器，Mac 和 Windows 编译桌面端不需要 Docker。

  ## 一、先在 Mac 上准备代码

  先确认项目可以完整构建：

  cd /Users/soulhappy/gitclone/qaplugin

  npm ci
  npm --prefix apps/cloud-web ci

  npm run check
  cargo test --workspace

  然后提交并推送代码：

  git status
  git add .
  git commit -m "prepare cloud and windows deployment"
  git push origin main

  .env.cloud、.env.desktop、本地数据库、截图和图片不会进入 Git。

  提前生成三个不同的凭据：

  openssl rand -hex 32
  openssl rand -hex 32
  openssl rand -hex 32

  分别用于：

  第一个：管理员初始密码
  第二个：Mac 设备 Token
  第三个：Windows 设备 Token

  例如规划设备：

  Mac 设备 ID：mac-dev
  Windows 设备 ID：win-test-01

  不要让 Mac 和 Windows 共用同一个设备 ID 或设备 Token。

  ## 二、部署云服务器

  建议配置：

  Ubuntu 24.04 LTS
  2 核 CPU
  4 GB 内存
  20 GB 以上磁盘
  一个域名，例如 qa.example.com

  ### 1. 配置域名

  在域名管理平台添加：

  类型：A
  主机记录：qa
  目标：云服务器公网 IPv4

  然后云服务器安全组开放：

  TCP 22
  TCP 80
  TCP 443
  UDP 443（可选）

  当前项目使用 Caddy 自动申请 HTTPS 证书，因此正式使用建议配置域名，并确保公网能够访问 80、443 端口。Caddy Automatic HTTPS
  (https://caddyserver.com/docs/automatic-https)

  不要把 6060 端口直接暴露到公网；它只在 Docker 内部由 Caddy 访问。

  ### 2. 安装 Git 和 Docker

  登录服务器：

  ssh root@服务器公网IP

  安装基础工具：

  sudo apt update
  sudo apt install -y git ca-certificates curl

  然后按照 Docker 官方 Ubuntu 文档添加 Docker 软件源，并安装：

  sudo apt install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin

  检查：

  sudo systemctl status docker
  sudo docker run hello-world
  docker compose version

  官方安装步骤见 Docker Engine for Ubuntu (https://docs.docker.com/engine/install/ubuntu/)。

  如果当前用户不能直接执行 Docker，可以暂时在后面的命令前加 sudo。

  ### 3. 下载项目

  cd /opt
  git clone git@github.com:LYDONLUCIEN/qaplugin.git
  cd qaplugin

  如果仓库是私有仓库，需要把服务器 SSH 公钥添加为 GitHub Deploy Key；也可以使用 HTTPS 克隆。

  ### 4. 创建云端配置

  cp .env.cloud.example .env.cloud
  nano .env.cloud

  使用 Qwen 时可以填写：

  QA_BIND_ADDR=0.0.0.0:6060
  QA_DOMAIN=qa.example.com
  QA_PUBLIC_PORT=6060

  QA_ADMIN_USERNAME=admin
  QA_ADMIN_PASSWORD=<第一个随机密码>
  QA_AUTH_SESSION_HOURS=168
  QA_COOKIE_SECURE=true

  QA_DEVICE_TOKENS=mac-dev=<Mac设备Token>,win-test-01=<Windows设备Token>
  QA_MAX_IMAGE_MB=20
  QA_DB_PATH=/app/data/qa-snapshot.db

  LLM_PROVIDER=qwen
  LLM_API_KEY=<阿里云百炼API-Key>
  LLM_MODEL=qwen3-vl-plus
  LLM_BASE_URL=https://dashscope.aliyuncs.com/compatible-mode
  LLM_MAX_TOKENS=1024

  如果希望由 DeepSeek V4 Flash Vision 直接理解截图，保持
  `QA_ANALYSIS_MODE=vision`，并将上面的全部 `LLM_*` 配置替换为：

  LLM_PROVIDER=deepseek-vision
  LLM_API_KEY=<DeepSeek API-Key>
  LLM_MODEL=deepseek-v4-flash-vision-exp
  LLM_BASE_URL=https://api.deepseek.com
  LLM_MAX_TOKENS=1024

  `deepseek-vision` 是视觉模型的明确选择；不要在视觉模式中只写
  `LLM_PROVIDER=deepseek`，它默认指向纯文本的 `deepseek-chat`。

  注意这里的对应关系：

  QA_ADMIN_PASSWORD
  └── 只用于网页 admin 首次登录

  QA_DEVICE_TOKENS
  ├── mac-dev=Mac客户端使用的Token
  └── win-test-01=Windows客户端使用的Token

  管理员密码不能当作设备 Token 使用。

  ### 5. 启动云服务

  ./start-cloud.sh deploy

  查看状态：

  ./start-cloud.sh status

  查看日志：

  ./start-cloud.sh logs

  脚本会根据 `QA_DOMAIN` 自动选择模式：真实域名启用 Caddy/HTTPS；留空或填写公网 IP 时通过 `QA_PUBLIC_PORT` 直接提供 HTTP，并自动使用非 Secure Cookie。无域名模式适合临时测试，长期公网使用仍建议配置域名和 HTTPS。

  验证：

  curl https://qa.example.com/healthz

  预期返回：

  {"ok":true,"version":"0.1.0"}

  随后浏览器打开：

  https://qa.example.com

  使用：

  用户名：admin
  密码：QA_ADMIN_PASSWORD 设置的值

  登录后建议立即：

  1. 修改 admin 密码。
  2. 创建普通用户。
  3. 将 win-test-01 或 mac-dev 分配给对应用户。
  4. 修改成功后，从服务器 .env.cloud 删除 QA_ADMIN_PASSWORD 明文。

  服务器会在 Docker Volume 中建立全新的数据库。Mac 本地的 data/qa-snapshot.db 不会自动上传；如果以后需要迁移旧历史，应单独做 SQLite 安
  全迁移。

  ## 三、在 Windows 上编译客户端

  Windows 客户端必须在 Windows 上编译。

  ### 1. 安装编译环境

  需要安装：

  - Git
  - Node.js LTS
  - Rust stable MSVC
  - Microsoft C++ Build Tools
  - Microsoft Edge WebView2 Runtime

  安装 Microsoft C++ Build Tools 时勾选：

  Desktop development with C++

  其中包含 MSVC 编译器和 Windows SDK。Tauri 官方也要求 Windows 开发环境具备 C++ Build Tools 和 WebView2；Windows 10 1803 以后通常已预装
  WebView2。Tauri Windows prerequisites (https://v2.tauri.app/start/prerequisites/)

  Rust 可以在 PowerShell 安装：

  winget install --id Rustlang.Rustup

  然后重新打开 PowerShell：

  rustup default stable-msvc
  rustc --version
  cargo --version
  node --version
  npm --version

  ### 2. 下载代码

  git clone git@github.com:LYDONLUCIEN/qaplugin.git
  cd qaplugin

  ### 3. 安装依赖并检查

  npm ci
  npm --prefix apps/cloud-web ci

  npm run check
  cargo test --workspace

  ### 4. 构建 Windows 安装包

  npm run tauri:build -- --bundles nsis

  构建成功后安装包一般位于：

  target\release\bundle\nsis\

  文件名类似：

  QA Snapshot_0.1.0_x64-setup.exe

  测试阶段安装未签名的程序时，Windows SmartScreen 可能提示风险，可以选择“更多信息 → 仍要运行”。正式分发给其他用户时，应购买并配置
  Windows Authenticode 代码签名证书。

  ### 5. 安装后配置 Windows 客户端

  运行安装包并打开 QA Snapshot，在 QA Control 中填写：

  API 地址：https://qa.example.com
  网页地址：https://qa.example.com
  设备 ID：win-test-01
  设备 Token：云端 QA_DEVICE_TOKENS 中 win-test-01 对应的 Token

  点击“保存并重新连接”。

  此时云端日志应该显示 Windows 设备已连接，网页设备列表也应该显示在线。

  不需要把 .env.cloud、模型 API Key 或管理员密码放到 Windows 电脑。

  ## 四、Mac 客户端连接正式云端

  如果继续用源码运行：

  cp .env.desktop.example .env.desktop

  填写：

  QA_CLOUD_URL=https://qa.example.com
  QA_WEB_URL=https://qa.example.com
  QA_DEVICE_ID=mac-dev
  QA_DEVICE_TOKEN=<Mac设备Token>

  然后：

  ./start.sh dev

  也可以构建 Mac 安装包：

  ./start.sh deploy

  产物位于：

  target/release/bundle/macos/
  target/release/bundle/dmg/

  ## 五、最终验收流程

  建议按以下顺序测试：

  1. 服务器 /healthz 正常。
  2. 网页能用 admin 登录。
  3. Windows 客户端显示“云端已连接”。
  4. 网页显示 win-test-01 在线。
  5. 网页点击截图。
  6. Windows 本地完成截图。
  7. 网页显示截图气泡。
  8. Qwen 返回 Markdown 流式回答。
  9. Windows Overlay 同步显示回答。
  10. 手机使用移动网络访问域名并触发截图。

  目前 Windows 程序关闭 Control 窗口后会隐藏并继续后台运行，但尚未实现系统开机自动启动；电脑重启后需要手动打开程序。项目现有部署说明也
  可以参考 DEPLOY.md。
