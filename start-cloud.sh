#!/usr/bin/env bash
# Local cloud API + cloud web launcher.

set -euo pipefail
cd "$(dirname "$0")"

if [[ -f .env.cloud ]]; then
  echo "→ sourcing .env.cloud"
  set -a; source .env.cloud; set +a
elif [[ -f .env ]]; then
  echo "→ sourcing legacy .env (create .env.cloud to isolate cloud config)"
  set -a; source .env; set +a
fi

if [[ -z "${QA_DEVICE_TOKENS:-}" && ( -z "${QA_DEVICE_ID:-}" || -z "${QA_DEVICE_TOKEN:-}" ) ]]; then
  echo "✗ Configure QA_DEVICE_TOKENS or QA_DEVICE_ID + QA_DEVICE_TOKEN."
  exit 1
fi

echo "→ building cloud web"
npm --prefix apps/cloud-web run build

echo "→ starting qa-api"
exec cargo run -p qa-api
