#!/usr/bin/env bash
# QA Snapshot desktop client manager.
#
# Commands:
#   dev      Run the Tauri development client in the foreground.
#   start    Start the packaged desktop application in the background.
#   stop     Stop the packaged application started from this repository.
#   restart  Stop and start the packaged application.
#   deploy   Build a platform installer/application bundle.
#   status   Show managed application status.
#   logs     Follow the packaged application log.

set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
RUNTIME_DIR="$ROOT_DIR/.run"
PID_FILE="$RUNTIME_DIR/desktop.pid"
LOG_FILE="$RUNTIME_DIR/desktop.log"
COMMAND="${1:-dev}"

cd "$ROOT_DIR"

usage() {
  cat <<'EOF'
Usage: ./start.sh <command>

Commands:
  dev       Run the Tauri client in the foreground; Ctrl+C stops it
  start     Start the packaged application in the background
  stop      Stop the packaged application managed by this repository
  restart   Restart the packaged application
  deploy    Build a macOS .app/.dmg or Windows NSIS installer
  status    Show whether the packaged application is running
  logs      Follow the packaged application log
  help      Show this help

Run './start.sh deploy' once before using start/restart.
EOF
}

load_environment() {
  if [[ -f "$ROOT_DIR/.env.desktop" ]]; then
    echo "→ sourcing .env.desktop"
    set -a
    # shellcheck disable=SC1091
    source "$ROOT_DIR/.env.desktop"
    set +a
  elif [[ -f "$ROOT_DIR/.env" ]]; then
    echo "→ sourcing legacy .env"
    set -a
    # shellcheck disable=SC1091
    source "$ROOT_DIR/.env"
    set +a
  fi

  if [[ -z "${QA_CLOUD_URL:-}" || -z "${QA_DEVICE_ID:-}" || -z "${QA_DEVICE_TOKEN:-}" ]]; then
    echo "ℹ No complete desktop environment config found."
    echo "  You can configure the cloud URL and device credentials in the Control window."
  fi
}

require_build_tools() {
  if ! command -v npm >/dev/null 2>&1; then
    echo "✗ npm not found. Install Node.js first."
    exit 1
  fi
  if ! command -v cargo >/dev/null 2>&1; then
    echo "✗ cargo not found. Install Rust stable first."
    exit 1
  fi
}

platform_name() {
  case "$(uname -s)" in
    Darwin) echo "macos" ;;
    MINGW*|MSYS*|CYGWIN*) echo "windows" ;;
    *) echo "unsupported" ;;
  esac
}

app_binary() {
  case "$(platform_name)" in
    macos)
      echo "$ROOT_DIR/target/release/bundle/macos/QA Snapshot.app/Contents/MacOS/qa-snapshot"
      ;;
    windows)
      echo "$ROOT_DIR/target/release/qa-snapshot.exe"
      ;;
    *)
      return 1
      ;;
  esac
}

app_bundle() {
  [[ "$(platform_name)" == "macos" ]] || return 1
  echo "$ROOT_DIR/target/release/bundle/macos/QA Snapshot.app"
}

pid_command() {
  local pid="$1"
  ps -p "$pid" -o command= 2>/dev/null || true
}

pid_is_managed_app() {
  local pid="$1"
  local expected command
  expected="$(app_binary 2>/dev/null || true)"
  [[ -n "$expected" ]] || return 1
  kill -0 "$pid" 2>/dev/null || return 1
  command="$(pid_command "$pid")"
  [[ "$command" == "$expected" || "$command" == "$expected "* ]]
}

read_managed_pid() {
  [[ -f "$PID_FILE" ]] || return 1
  local pid
  pid="$(tr -d '[:space:]' < "$PID_FILE")"
  [[ "$pid" =~ ^[0-9]+$ ]] || return 1
  pid_is_managed_app "$pid" || return 1
  echo "$pid"
}

discover_managed_pid() {
  local expected pid command
  expected="$(app_binary 2>/dev/null || true)"
  [[ -n "$expected" ]] || return 1
  while IFS= read -r pid; do
    [[ "$pid" =~ ^[0-9]+$ ]] || continue
    command="$(pid_command "$pid")"
    if [[ "$command" == "$expected" || "$command" == "$expected "* ]]; then
      echo "$pid"
      return 0
    fi
  done < <(pgrep -f "$expected" 2>/dev/null || true)
  return 1
}

managed_pid() {
  local pid
  if pid="$(read_managed_pid)"; then
    echo "$pid"
    return 0
  fi
  rm -f "$PID_FILE"
  if pid="$(discover_managed_pid)"; then
    mkdir -p "$RUNTIME_DIR"
    echo "$pid" > "$PID_FILE"
    echo "$pid"
    return 0
  fi
  return 1
}

start_app() {
  local binary bundle pid waited
  local -a open_args
  binary="$(app_binary)" || {
    echo "✗ Packaged desktop start is currently supported on macOS and Windows."
    exit 1
  }
  if pid="$(managed_pid)"; then
    echo "✓ QA Snapshot is already running (PID $pid)."
    return
  fi
  if [[ ! -x "$binary" ]]; then
    echo "✗ Packaged application not found: $binary"
    echo "  Build it first with: ./start.sh deploy"
    exit 1
  fi
  mkdir -p "$RUNTIME_DIR"
  echo "→ starting packaged QA Snapshot"
  if [[ "$(platform_name)" == "macos" ]]; then
    bundle="$(app_bundle)"
    open_args=(-n -o "$LOG_FILE" --stderr "$LOG_FILE")
    [[ -n "${QA_CLOUD_URL:-}" ]] && open_args+=(--env "QA_CLOUD_URL=${QA_CLOUD_URL}")
    [[ -n "${QA_WEB_URL:-}" ]] && open_args+=(--env "QA_WEB_URL=${QA_WEB_URL}")
    [[ -n "${QA_DEVICE_ID:-}" ]] && open_args+=(--env "QA_DEVICE_ID=${QA_DEVICE_ID}")
    [[ -n "${QA_DEVICE_TOKEN:-}" ]] && open_args+=(--env "QA_DEVICE_TOKEN=${QA_DEVICE_TOKEN}")
    open "${open_args[@]}" "$bundle"
    waited=0
    pid=""
    while [[ -z "$pid" && "$waited" -lt 5 ]]; do
      sleep 1
      pid="$(discover_managed_pid || true)"
      waited=$((waited + 1))
    done
  else
    nohup "$binary" >> "$LOG_FILE" 2>&1 &
    pid=$!
    sleep 1
  fi
  if [[ -z "$pid" ]]; then
    echo "✗ QA Snapshot process was not detected after launch. Check $LOG_FILE"
    exit 1
  fi
  echo "$pid" > "$PID_FILE"
  if ! pid_is_managed_app "$pid"; then
    rm -f "$PID_FILE"
    echo "✗ QA Snapshot exited during startup. Check $LOG_FILE"
    exit 1
  fi
  echo "✓ QA Snapshot started (PID $pid)."
}

stop_app() {
  local pid waited
  if ! pid="$(managed_pid)"; then
    echo "✓ QA Snapshot is not running from this repository."
    return
  fi
  echo "→ stopping QA Snapshot (PID $pid)"
  kill -TERM "$pid"
  waited=0
  while kill -0 "$pid" 2>/dev/null && (( waited < 10 )); do
    sleep 1
    waited=$((waited + 1))
  done
  if kill -0 "$pid" 2>/dev/null; then
    echo "✗ QA Snapshot did not stop within 10 seconds."
    echo "  Refusing to force-kill it automatically; inspect PID $pid manually."
    exit 1
  fi
  rm -f "$PID_FILE"
  echo "✓ QA Snapshot stopped."
}

run_dev() {
  require_build_tools
  echo "→ starting tauri development client"
  echo "  Press Ctrl+C to stop."
  exec npm --prefix "$ROOT_DIR" run tauri:dev
}

build_package() {
  local platform
  require_build_tools
  platform="$(platform_name)"
  case "$platform" in
    macos)
      echo "→ building macOS application and DMG"
      npm --prefix "$ROOT_DIR" run tauri:build -- --bundles app,dmg
      echo "✓ app: $ROOT_DIR/target/release/bundle/macos/QA Snapshot.app"
      echo "✓ dmg: $ROOT_DIR/target/release/bundle/dmg/"
      ;;
    windows)
      echo "→ building Windows NSIS installer"
      npm --prefix "$ROOT_DIR" run tauri:build -- --bundles nsis
      echo "✓ installer directory: $ROOT_DIR/target/release/bundle/nsis/"
      ;;
    *)
      echo "✗ Packaging is configured for macOS and Windows only."
      exit 1
      ;;
  esac
}

case "$COMMAND" in
  help|-h|--help)
    usage
    exit 0
    ;;
  dev|start|stop|restart|deploy|status|logs)
    ;;
  *)
    echo "✗ Unknown command: $COMMAND"
    usage
    exit 2
    ;;
esac

load_environment

case "$COMMAND" in
  dev)
    run_dev
    ;;
  start)
    start_app
    ;;
  stop)
    stop_app
    ;;
  restart)
    stop_app
    start_app
    ;;
  deploy)
    build_package
    ;;
  status)
    if pid="$(managed_pid)"; then
      echo "✓ QA Snapshot is running (PID $pid)."
    else
      echo "○ QA Snapshot is not running from this repository."
    fi
    ;;
  logs)
    mkdir -p "$RUNTIME_DIR"
    touch "$LOG_FILE"
    tail -f "$LOG_FILE"
    ;;
esac
