#!/usr/bin/env bash
# Desktop development launcher. The desktop only knows the cloud URL and
# its own device credentials; LLM provider keys belong on the cloud service.

set -euo pipefail

cd "$(dirname "$0")"

if [[ -f .env.desktop ]]; then
  echo "→ sourcing .env.desktop"
  set -a; source .env.desktop; set +a
elif [[ -f .env ]]; then
  echo "→ sourcing legacy .env"
  set -a; source .env; set +a
fi

if [[ -z "${QA_CLOUD_URL:-}" || -z "${QA_DEVICE_ID:-}" || -z "${QA_DEVICE_TOKEN:-}" ]]; then
  echo "ℹ No complete desktop environment config found."
  echo "  You can configure the cloud URL and device credentials in the Control window."
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "✗ cargo not found. Install Rust first:"
  echo "    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
  exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "(optional) install jq for nicer logs"
fi

echo "→ starting tauri:dev"
exec npm run tauri:dev
