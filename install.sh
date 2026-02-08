#!/usr/bin/env bash
# Instalação Rusty Proxy (Opção B - EnvironmentFile) - corrigido

set -Eeuo pipefail
IFS=$'\n\t'

TOTAL_STEPS=10
CURRENT_STEP=0

LOG_FILE="/var/log/rustyproxy-install.log"

RUSTY_DIR="/opt/rustyproxy"
ENV_DIR="/etc/rustyproxy"
UNIT_TEMPLATE="/etc/systemd/system/proxy@.service"
PORTS_FILE="${RUSTY_DIR}/ports"

CLONE_DIR="/root/RustyProxyOnly"
REPO_URL='https://github.com/PhoenixxZ2023/RustyProxyOnly.git'
REPO_REF="${RUSTYPROXY_REF:-main}"          # export RUSTYPROXY_REF=<tag|commit>
DO_UPGRADE="${RUSTYPROXY_UPGRADE:-0}"       # 1 = faz apt upgrade

show_progress() {
  local msg="$1"
  local percent=$(( CURRENT_STEP * 100 / TOTAL_STEPS ))
  echo "Progresso: [${percent}%] - ${msg}"
}

error_exit() {
  echo -e "\nErro: $1"
  echo -e "\n--- Últimas linhas do log (${LOG_FILE}) ---"
  tail -n 120 "${LOG_FILE}" 2>/dev/null || true
  exit 1
}

increment_step() { CURRENT_STEP=$((CURRENT_STEP + 1)); }

trap 'error_exit "Falha na linha $LINENO."' ERR

run() { "$@" >>"${LOG_FILE}" 2>&1; }

require_root() {
  [[ "${EUID}" -eq 0 ]] || error_exit "EXECUTE COMO ROOT (sudo)."
}

detect_os() {
  if ! command -v lsb_release >/dev/null 2>&1; then
    run apt-get update -y
    run apt-get install -y lsb-release
  fi

  local os ver
  os="$(lsb_release -is)"
  ver="$(lsb_release -rs)"

  case "${os}" in
    Ubuntu)
      case "${ver}" in
        24.*|22.*|20.*|18.*) : ;;
        *) error_exit "UBUNTU NÃO SUPORTADO. USE 18/20/22/24." ;;
      esac
      ;;
    Debian)
      case "${ver}" in
        12*|11*|10*|9*) : ;;
        *) error_exit "DEBIAN NÃO SUPORTADO. USE 9/10/11/12." ;;
      esac
      ;;
    *) error_exit "SISTEMA NÃO SUPORTADO. USE UBUNTU OU DEBIAN." ;;
  esac
}

ensure_deps() {
  export DEBIAN_FRONTEND=noninteractive
  run apt-get update -y
  if [[ "${DO_UPGRADE}" == "1" ]]; then
    run apt-get upgrade -y
  fi
  run apt-get install -y curl build-essential git pkg-config libssl-dev ca-certificates
}

ensure_rust() {
  if command -v cargo >/dev/null 2>&1; then
    return 0
  fi

  local tmp
  tmp="$(mktemp -t rustup-init.XXXXXX.sh)"
  run curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o "${tmp}"
  run sh "${tmp}" -y --profile minimal --no-modify-path
  rm -f "${tmp}"

  if [[ -f "/root/.cargo/env" ]]; then
    # shellcheck disable=SC1091
    source "/root/.cargo/env"
  fi
  command -v cargo >/dev/null 2>&1 || error_exit "Falha ao disponibilizar cargo após rustup."
}

install_unit_template() {
  # template systemd para proxy@PORT.service
  cat >"${UNIT_TEMPLATE}" <<'EOF'
[Unit]
Description=RustyProxy instance on port %i
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
EnvironmentFile=/etc/rustyproxy/proxy%i.env
ExecStart=/opt/rustyproxy/proxy --port %i --status ${STATUS}
Restart=always
RestartSec=2
LimitNOFILE=1048576

# Hardening (ajuste se precisar)
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
LockPersonality=true
RestrictSUIDSGID=true
RestrictRealtime=true
MemoryDenyWriteExecute=true

[Install]
WantedBy=multi-user.target
EOF

  run systemctl daemon-reload
}

install_cli() {
  cat >/usr/local/bin/rustyproxyctl <<'EOF'
#!/usr/bin/env bash
set -Eeuo pipefail
IFS=$'\n\t'

PORTS_FILE="/opt/rustyproxy/ports"

usage() {
  cat <<USAGE
Uso:
  rustyproxyctl list        # lista portas + status salvo
  rustyproxyctl status      # mostra estado systemd de cada porta
  rustyproxyctl logs <port> # logs do serviço (últimas 200 linhas)
USAGE
}

cmd="${1:-status}"

case "$cmd" in
  list)
    if [[ ! -s "$PORTS_FILE" ]]; then
      echo "Nenhuma porta cadastrada."
      exit 0
    fi
    echo "PORTA | STATUS"
    echo "--------------"
    cat "$PORTS_FILE"
    ;;
  status)
    if [[ ! -s "$PORTS_FILE" ]]; then
      echo "Nenhuma porta cadastrada."
      exit 0
    fi
    while IFS='|' read -r port status; do
      [[ -n "${port:-}" ]] || continue
      svc="proxy@${port}.service"
      state="$(systemctl is-active "$svc" 2>/dev/null || true)"
      enabled="$(systemctl is-enabled "$svc" 2>/dev/null || true)"
      printf "port=%s  active=%s  enabled=%s  status=%s\n" "$port" "$state" "$enabled" "${status:-}"
    done < "$PORTS_FILE"
    ;;
  logs)
    port="${2:-}"
    [[ "$port" =~ ^[0-9]+$ ]] || { echo "Informe uma porta válida."; exit 1; }
    journalctl -u "proxy@${port}.service" -n 200 --no-pager
    ;;
  *)
    usage
    exit 1
    ;;
esac
EOF

  chmod 0755 /usr/local/bin/rustyproxyctl
}

setup_links() {
  # (NOVO) garante permissões + link simbólico
  run chmod 0755 "${RUSTY_DIR}/menu"
  run ln -sf "${RUSTY_DIR}/menu" /usr/local/bin/rustyproxy

  # garante também a permissão do CLI
  if [[ -f /usr/local/bin/rustyproxyctl ]]; then
    run chmod 0755 /usr/local/bin/rustyproxyctl
  fi
}

install_rustyproxy() {
  rm -rf "${CLONE_DIR}" || true
  run git clone --branch "${REPO_REF}" "${REPO_URL}" "${CLONE_DIR}"

  run mkdir -p "${RUSTY_DIR}" "${ENV_DIR}"

  # instala menu (o menu deve ser o da Opção B)
  [[ -f "${CLONE_DIR}/menu.sh" ]] || error_exit "menu.sh não encontrado no repo."
  run install -m 0755 "${CLONE_DIR}/menu.sh" "${RUSTY_DIR}/menu"

  # build Rust
  [[ -d "${CLONE_DIR}/RustyProxy" ]] || error_exit "Diretório Rust não encontrado: ${CLONE_DIR}/RustyProxy"
  pushd "${CLONE_DIR}/RustyProxy" >/dev/null
  run cargo build --release --jobs "$(nproc)"
  popd >/dev/null

  # --- detectar o binário automaticamente ---
  local release_dir="${CLONE_DIR}/RustyProxy/target/release"
  local bin_path=""

  bin_path="$(find "${release_dir}" -maxdepth 1 -type f -executable \
    ! -name '*.d' ! -name '*.rlib' ! -name '*.so' ! -name '*.a' 2>/dev/null | head -n 1 || true)"

  [[ -n "${bin_path}" ]] || error_exit "Nenhum binário executável encontrado em ${release_dir}"

  run install -m 0755 "${bin_path}" "${RUSTY_DIR}/proxy"

  # arquivos de controle
  run touch "${PORTS_FILE}"
  run chmod 600 "${PORTS_FILE}"
  run chmod 700 "${ENV_DIR}"
}

cleanup() {
  rm -rf "${CLONE_DIR}" || true
}

main() {
  require_root
  : > "${LOG_FILE}"

  clear
  echo ""
  echo -e "\033[0;34m           ╦═╗╦ ╦╔═╗╔╦╗╦ ╦  ╔═╗╦═╗╔═╗═╗ ╦╦ ╦"
  echo -e "\033[0;37m           ╠╦╝║ ║╚═╗ ║ ╚╦╝  ╠═╝╠╦╝║ ║╔╩╦╝╚╦╝"
  echo -e "\033[0;34m           ╩╚═╚═╝╚═╝ ╩  ╩   ╩  ╩╚═╚═╝╩ ╚═ ╩"
  echo -e " "

  show_progress "VERIFICANDO SISTEMA..."
  detect_os
  increment_step

  show_progress "INSTALANDO DEPENDÊNCIAS..."
  ensure_deps
  increment_step

  show_progress "INSTALANDO RUST (se necessário)..."
  ensure_rust
  increment_step

  show_progress "CLONANDO E COMPILANDO..."
  install_rustyproxy
  increment_step

  show_progress "INSTALANDO TEMPLATE SYSTEMD (proxy@PORT)..."
  install_unit_template
  increment_step

  show_progress "INSTALANDO rustyproxyctl (CLI)..."
  install_cli
  increment_step

  show_progress "CRIANDO LINK DO MENU (rustyproxy)..."
  setup_links
  increment_step

  show_progress "LIMPANDO..."
  cleanup
  increment_step

  # (NOVO) Mensagem final limpa e bonita
  clear
  echo -e " "
  echo -e "\033[1;34m==============================================================\033[0m"
  echo -e "\033[1;34m\033[1m                INSTALAÇÃO CONCLUÍDA COM SUCESSO               \033[0m"
  echo -e "\033[1;34m==============================================================\033[0m"
  echo -e " "
  echo -e "\033[1;34m\033[1mDIGITE:\033[0m \033[1;37mrustyproxy\033[0m"
  echo -e " "
}

main "$@"
