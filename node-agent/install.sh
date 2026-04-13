#!/usr/bin/env bash

set -euo pipefail

REPO="${COLONYOS_REPO:-ant-pm/node-agent}"
BIN_NAME="node-agent"
INSTALL_DIR="/usr/local/bin"
SERVICE_NAME="node-agent"
ENV_FILE="/etc/colonyos/node-agent.env"
KEY_DIR="/var/lib/colonyos/node-agent"

# --- output helpers ---

RED='\033[31m'; GREEN='\033[32m'; YELLOW='\033[33m'; CYAN='\033[36m'; BOLD='\033[1m'; RESET='\033[0m'

ok()   { printf "${GREEN}✔${RESET}  %s\n" "$*"; }
fail() { printf "${RED}✘${RESET}  %s\n" "$*" >&2; exit 1; }
info() { printf "${CYAN}→${RESET}  %s\n" "$*"; }
warn() { printf "${YELLOW}!${RESET}  %s\n" "$*"; }
step() { printf "\n${BOLD}%s${RESET}\n" "$*"; }

# --- checks ---

step "Checking dependencies"

if command -v docker >/dev/null 2>&1; then
    ok "docker found ($(docker --version | head -1))"
else
    fail "docker is required but not found in PATH"
fi

if command -v curl >/dev/null 2>&1; then
    ok "curl found"
else
    fail "curl is required but not found in PATH"
fi

HAS_SYSTEMD=0
if [ -d /run/systemd/system ] && command -v systemctl >/dev/null 2>&1; then
    HAS_SYSTEMD=1
    ok "systemd detected"
else
    warn "systemd not detected — will run ephemerally"
fi

# --- prompts (readline: arrows, copy/paste, editing) ---

# `read -e` enables readline; `</dev/tty` ensures a real tty even if piped.

prompt() {
    local var="$1" msg="$2" silent="${3:-0}" ans
    if [ "$silent" = "1" ]; then
        IFS= read -rsp "$msg" ans </dev/tty; echo
    else
        IFS= read -rep "$msg" ans </dev/tty
    fi
    printf -v "$var" '%s' "$ans"
}

prompt_yn() {
    local var="$1" msg="$2" ans
    while :; do
        IFS= read -rep "$msg [y/N]: " ans </dev/tty
        case "${ans,,}" in
            y|yes) printf -v "$var" 'y'; return ;;
            n|no|"") printf -v "$var" 'n'; return ;;
        esac
    done
}

step "Configuration"

prompt COLONY_HOST        "  Colony host: "
prompt COLONY             "  Colony name: "
prompt COLONY_PRIVATE_KEY "  Colony private key: " 1
prompt_yn ENABLE_METRICS  "  Enable metrics?"

[ "$ENABLE_METRICS" = "y" ] && METRICS_VAL=true || METRICS_VAL=false

if [ "$HAS_SYSTEMD" = "1" ]; then
    prompt_yn INSTALL_SVC "  Install as systemd service?"
else
    INSTALL_SVC=n
fi

[ -z "$COLONY_HOST" ]        && fail "colony host cannot be empty"
[ -z "$COLONY" ]             && fail "colony name cannot be empty"
[ -z "$COLONY_PRIVATE_KEY" ] && fail "colony private key cannot be empty"

# --- arch detection ---

step "Detecting platform"

case "$(uname -m)" in
    x86_64|amd64)  ARCH=amd64 ;;
    aarch64|arm64) ARCH=arm64 ;;
    *) fail "unsupported arch: $(uname -m)" ;;
esac

ok "linux-$ARCH"

# --- download latest release binary ---

step "Downloading"

ASSET="${BIN_NAME}-linux-${ARCH}"
URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

info "$URL"
curl -fSL --progress-bar -o "$TMP/$BIN_NAME" "$URL" || fail "download failed"
chmod +x "$TMP/$BIN_NAME"
ok "binary downloaded"

SUDO=""; [ "$(id -u)" -ne 0 ] && SUDO="sudo"

if [ "$INSTALL_SVC" = "y" ]; then
    step "Installing"

    $SUDO install -m 0755 "$TMP/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME"
    ok "binary installed to $INSTALL_DIR/$BIN_NAME"

    $SUDO install -d -m 0700 "$KEY_DIR"
    ok "key directory created ($KEY_DIR)"

    $SUDO install -d -m 0755 "$(dirname "$ENV_FILE")"
    TMP_ENV="$(mktemp)"
    cat >"$TMP_ENV" <<EOF
COLONY_HOST=${COLONY_HOST}
COLONY=${COLONY}
COLONY_PRIVATE_KEY=${COLONY_PRIVATE_KEY}
ENABLE_METRICS=${METRICS_VAL}
EOF
    $SUDO install -m 0600 "$TMP_ENV" "$ENV_FILE"
    rm -f "$TMP_ENV"
    ok "env file written ($ENV_FILE)"

    step "Setting up systemd service"

    UNIT="/etc/systemd/system/${SERVICE_NAME}.service"
    $SUDO tee "$UNIT" >/dev/null <<EOF
[Unit]
Description=ColonyOS Node Agent
After=network-online.target docker.service
Wants=network-online.target
Requires=docker.service

[Service]
Type=simple
EnvironmentFile=${ENV_FILE}
ExecStart=${INSTALL_DIR}/${BIN_NAME}
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
    ok "unit file written ($UNIT)"

    info "reloading systemd daemon"
    $SUDO systemctl daemon-reload
    ok "daemon reloaded"

    info "enabling and starting ${SERVICE_NAME}.service"
    $SUDO systemctl enable --now "${SERVICE_NAME}.service"
    ok "service enabled"

    printf "\n🚀  ${BOLD}${GREEN}Node agent is running!${RESET}\n"
    printf "    systemctl status %s\n\n" "$SERVICE_NAME"
else
    step "Launching"
    mkdir -p "$KEY_DIR"
    $SUDO install -m 0755 "$TMP/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME"
    ok "binary installed to $INSTALL_DIR/$BIN_NAME"
    printf "\n🚀  ${BOLD}${GREEN}Starting node agent${RESET} (Ctrl-C to stop)\n\n"
    env \
        COLONY_HOST="$COLONY_HOST" \
        COLONY="$COLONY" \
        COLONY_PRIVATE_KEY="$COLONY_PRIVATE_KEY" \
        ENABLE_METRICS="$METRICS_VAL" \
        "$INSTALL_DIR/$BIN_NAME"
fi
