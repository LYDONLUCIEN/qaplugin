#!/usr/bin/env bash
# QA Snapshot cloud service manager.
#
# Commands:
#   dev      Build and run qa-api in the foreground without Docker.
#   start    Start the existing Docker deployment.
#   stop     Stop Docker services while preserving containers and data.
#   restart  Recreate Docker services so environment changes take effect.
#   deploy   Rebuild images and recreate Docker services.
#   status   Show the selected mode and Docker service status.
#   logs     Follow qa-api logs (and Caddy logs in domain mode).

set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ENV_FILE="$ROOT_DIR/.env.cloud"
LEGACY_ENV_FILE="$ROOT_DIR/.env"
COMPOSE_FILE="$ROOT_DIR/deploy/docker-compose.yml"
IP_COMPOSE_FILE="$ROOT_DIR/deploy/docker-compose.ip.yml"
COMMAND="${1:-dev}"

cd "$ROOT_DIR"

usage() {
  cat <<'EOF'
Usage: ./start-cloud.sh <command>

Commands:
  dev       Run cloud web + qa-api in the foreground (no Docker)
  start     Start the Docker deployment without forcing a rebuild
  stop      Stop cloud containers; database volumes are preserved
  restart   Recreate containers and reload .env.cloud
  deploy    Rebuild images and recreate containers
  status    Show deployment mode and container status
  logs      Follow cloud logs
  help      Show this help

Deployment mode is selected automatically from QA_DOMAIN:
  valid hostname  -> Caddy + HTTPS on ports 80/443, Secure cookie
  empty/IP value  -> direct HTTP on QA_PUBLIC_PORT (default 6060), non-Secure cookie

For public production use, a domain with HTTPS is strongly recommended.
EOF
}

load_environment() {
  if [[ -f "$ENV_FILE" ]]; then
    echo "→ sourcing .env.cloud"
    set -a
    # shellcheck disable=SC1090
    source "$ENV_FILE"
    set +a
  elif [[ "$COMMAND" == "dev" && -f "$LEGACY_ENV_FILE" ]]; then
    echo "→ sourcing legacy .env (create .env.cloud to isolate cloud config)"
    set -a
    # shellcheck disable=SC1090
    source "$LEGACY_ENV_FILE"
    set +a
  else
    echo "✗ Missing $ENV_FILE"
    echo "  Create it from .env.cloud.example before using '$COMMAND'."
    exit 1
  fi
}

validate_cloud_config() {
  if [[ -z "${QA_DEVICE_TOKENS:-}" && ( -z "${QA_DEVICE_ID:-}" || -z "${QA_DEVICE_TOKEN:-}" ) ]]; then
    echo "✗ Configure QA_DEVICE_TOKENS or QA_DEVICE_ID + QA_DEVICE_TOKEN."
    exit 1
  fi
}

is_ipv4_address() {
  local value="$1"
  [[ "$value" =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ ]]
}

has_valid_domain() {
  local value="${QA_DOMAIN:-}"
  local label
  local -a labels
  [[ -n "$value" ]] || return 1
  [[ "$value" != "qa.example.com" && "$value" != "example.com" && "$value" != "localhost" ]] || return 1
  [[ "$value" != *"://"* && "$value" != */* && "$value" != *:* ]] || return 1
  is_ipv4_address "$value" && return 1
  [[ "$value" == *.* ]] || return 1
  IFS='.' read -r -a labels <<< "$value"
  for label in "${labels[@]}"; do
    [[ "$label" =~ ^[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?$ ]] || return 1
  done
  return 0
}

detect_mode() {
  QA_PUBLIC_PORT="${QA_PUBLIC_PORT:-6060}"
  export QA_PUBLIC_PORT
  if has_valid_domain; then
    DEPLOYMENT_MODE="domain"
    QA_EFFECTIVE_COOKIE_SECURE="true"
    PUBLIC_URL="https://${QA_DOMAIN}"
  else
    DEPLOYMENT_MODE="ip"
    QA_EFFECTIVE_COOKIE_SECURE="false"
    if [[ -n "${QA_DOMAIN:-}" ]] && is_ipv4_address "$QA_DOMAIN"; then
      PUBLIC_URL="http://${QA_DOMAIN}:${QA_PUBLIC_PORT}"
    else
      PUBLIC_URL="http://SERVER_IP:${QA_PUBLIC_PORT}"
    fi
  fi
  export QA_EFFECTIVE_COOKIE_SECURE
}

print_mode() {
  if [[ "$DEPLOYMENT_MODE" == "domain" ]]; then
    echo "→ deployment mode: domain HTTPS ($PUBLIC_URL)"
    if [[ "${QA_COOKIE_SECURE:-}" != "true" ]]; then
      echo "  QA_COOKIE_SECURE is being overridden to true for HTTPS deployment."
    fi
  else
    echo "→ deployment mode: IP/HTTP ($PUBLIC_URL)"
    if [[ "${QA_COOKIE_SECURE:-}" != "false" ]]; then
      echo "  QA_COOKIE_SECURE is being overridden to false because no valid domain is configured."
    fi
    echo "  Warning: public HTTP does not protect login cookies or uploaded screenshots."
  fi
}

require_docker() {
  if ! command -v docker >/dev/null 2>&1; then
    echo "✗ Docker is required for '$COMMAND'. Use './start-cloud.sh dev' for foreground development."
    exit 1
  fi
  if ! docker compose version >/dev/null 2>&1; then
    echo "✗ Docker Compose Plugin is not available."
    exit 1
  fi
}

compose_current() {
  if [[ "$DEPLOYMENT_MODE" == "domain" ]]; then
    docker compose --env-file "$ENV_FILE" --profile domain -f "$COMPOSE_FILE" "$@"
  else
    docker compose --env-file "$ENV_FILE" -f "$COMPOSE_FILE" -f "$IP_COMPOSE_FILE" "$@"
  fi
}

compose_all() {
  docker compose --env-file "$ENV_FILE" --profile domain -f "$COMPOSE_FILE" -f "$IP_COMPOSE_FILE" "$@"
}

stop_obsolete_proxy() {
  if [[ "$DEPLOYMENT_MODE" == "ip" ]]; then
    # A previous domain deployment may have left Caddy running. It must not
    # continue to own ports 80/443 after switching to IP-only mode.
    compose_all stop caddy >/dev/null 2>&1 || true
  fi
}

run_dev() {
  if ! command -v npm >/dev/null 2>&1; then
    echo "✗ npm not found. Install Node.js first."
    exit 1
  fi
  if ! command -v cargo >/dev/null 2>&1; then
    echo "✗ cargo not found. Install Rust stable first."
    exit 1
  fi

  echo "→ development mode: foreground HTTP"
  echo "  Press Ctrl+C to stop."
  export QA_COOKIE_SECURE="false"
  echo "→ building cloud web"
  npm --prefix "$ROOT_DIR/apps/cloud-web" run build
  echo "→ building qa-api"
  cargo build --manifest-path "$ROOT_DIR/Cargo.toml" -p qa-api
  echo "→ starting qa-api"
  exec "$ROOT_DIR/target/debug/qa-api"
}

start_deployment() {
  stop_obsolete_proxy
  echo "→ starting cloud services"
  compose_current up -d --remove-orphans
  compose_current ps
  echo "✓ cloud URL: $PUBLIC_URL"
}

restart_deployment() {
  stop_obsolete_proxy
  echo "→ recreating cloud services and reloading .env.cloud"
  compose_current up -d --force-recreate --remove-orphans
  compose_current ps
  echo "✓ cloud URL: $PUBLIC_URL"
}

deploy() {
  stop_obsolete_proxy
  echo "→ rebuilding and deploying cloud services"
  compose_current up -d --build --force-recreate --remove-orphans
  compose_current ps
  echo "✓ cloud URL: $PUBLIC_URL"
  if [[ "$DEPLOYMENT_MODE" == "ip" ]]; then
    echo "  Set desktop QA_CLOUD_URL and QA_WEB_URL to $PUBLIC_URL"
  fi
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
validate_cloud_config
detect_mode
print_mode

case "$COMMAND" in
  dev)
    run_dev
    ;;
  start)
    require_docker
    start_deployment
    ;;
  stop)
    require_docker
    echo "→ stopping cloud services (data volumes are preserved)"
    compose_all stop
    ;;
  restart)
    require_docker
    restart_deployment
    ;;
  deploy)
    require_docker
    deploy
    ;;
  status)
    require_docker
    compose_all ps -a
    ;;
  logs)
    require_docker
    if [[ "$DEPLOYMENT_MODE" == "domain" ]]; then
      compose_current logs -f qa-api caddy
    else
      compose_current logs -f qa-api
    fi
    ;;
esac
