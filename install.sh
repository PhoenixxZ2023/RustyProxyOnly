#!/bin/bash
# Instalador Seguro RustyProxy v2.0

TOTAL_STEPS=9
CURRENT_STEP=0
LOG_FILE="/tmp/rustyproxy_install.log"

# Configuração de cores
GREEN='\033[1;32m'
RED='\033[1;31m'
YELLOW='\033[1;33m'
NC='\033[0m'
BOLD='\033[1m'

# Configuração de segurança
RUSTUP_URL="https://sh.rustup.rs"
RUSTUP_SHA256="a28077d6fd7b7376b65a8e9036f7e3a0c58f1e10e848b2555d97e3131a1137f3"
REPO_URL="https://github.com/PhoenixxZ2023/RustyProxyOnly.git"

# Funções de utilidade
show_progress() {
    PERCENT=$((CURRENT_STEP * 100 / TOTAL_STEPS))
    echo -e "${GREEN}${BOLD}[${PERCENT}%] ${NC}${BOLD}$1${NC}"
    logger "RustyProxy Installer: $1"
}

error_exit() {
    echo -e "\n${RED}${BOLD}ERRO CRÍTICO:${NC} $1" >&2
    logger "RustyProxy Installer Error: $1"
    exit 1
}

safe_exit() {
    echo -e "\n${YELLOW}${BOLD}Instalação interrompida. Realizando limpeza...${NC}"
    rm -rf /root/RustyProxyOnly
    exit 1
}

increment_step() {
    CURRENT_STEP=$((CURRENT_STEP + 1))
}

# Verificação inicial
[[ $EUID -ne 0 ]] && error_exit "Este script deve ser executado como root"
trap safe_exit SIGINT SIGTERM

# Verificação de arquitetura
ARCH=$(uname -m)
[[ "$ARCH" != "x86_64" && "$ARCH" != "aarch64" ]] && error_exit "Arquitetura não suportada: $ARCH"

# Verificação de dependências
check_dependencies() {
    local deps=("curl" "git" "build-essential" "gcc" "make" "pkg-config")
    local missing=()
    
    for dep in "${deps[@]}"; do
        if ! command -v $dep &>/dev/null; then
            missing+=("$dep")
        fi
    done

    if [[ ${#missing[@]} -gt 0 ]]; then
        show_progress "Instalando dependências faltantes: ${missing[*]}"
        apt-get install -y "${missing[@]}" > "$LOG_FILE" 2>&1 || error_exit "Falha ao instalar dependências"
    fi
}

# Execução principal
{
    clear
    show_progress "Iniciando instalação do RustyProxy"
    
    # Etapa 1: Atualização do sistema
    show_progress "Atualizando repositórios do sistema"
    apt-get update -y > "$LOG_FILE" 2>&1 || error_exit "Falha na atualização dos repositórios"
    increment_step

    # Etapa 2: Verificação do sistema
    show_progress "Verificando compatibilidade do sistema"
    check_dependencies
    
    OS_INFO=$(lsb_release -is 2>/dev/null || echo "Unknown")
    OS_VERSION=$(lsb_release -rs 2>/dev/null || echo "0")
    
    case "$OS_INFO" in
        Ubuntu)
            [[ "$OS_VERSION" =~ ^(18|20|22|24) ]] || error_exit "Versão do Ubuntu não suportada"
            ;;
        Debian)
            [[ "$OS_VERSION" =~ ^(9|10|11|12) ]] || error_exit "Versão do Debian não suportada"
            ;;
        *)
            error_exit "Sistema operacional não suportado"
            ;;
    esac
    increment_step

    # Etapa 3: Atualização do sistema
    show_progress "Atualizando pacotes do sistema"
    apt-get full-upgrade -y > "$LOG_FILE" 2>&1 || error_exit "Falha na atualização do sistema"
    increment_step

    # Etapa 4: Criação de diretórios
    show_progress "Criando estrutura de diretórios"
    mkdir -p /opt/rustyproxy/{bin,config} > "$LOG_FILE" 2>&1
    chmod 750 /opt/rustyproxy > "$LOG_FILE" 2>&1
    increment_step

    # Etapa 5: Instalação do Rust
    show_progress "Instalando Rust Toolchain"
    if ! command -v rustc &>/dev/null; then
        curl -sSf $RUSTUP_URL -o /tmp/rustup.sh > "$LOG_FILE" 2>&1
        echo "$RUSTUP_SHA256 /tmp/rustup.sh" | sha256sum -c - > "$LOG_FILE" 2>&1 || error_exit "Verificação de integridade falhou"
        sh /tmp/rustup.sh -y --default-toolchain stable --profile minimal > "$LOG_FILE" 2>&1
        source "$HOME/.cargo/env" > "$LOG_FILE" 2>&1
    fi
    increment_step

    # Etapa 6: Compilação do RustyProxy
    show_progress "Compilando RustyProxy (Isso pode levar vários minutos)"
    git clone $REPO_URL /tmp/RustyProxyOnly > "$LOG_FILE" 2>&1 || error_exit "Falha ao clonar repositório"
    
    cd /tmp/RustyProxyOnly/RustyProxy || error_exit "Diretório do projeto não encontrado"
    cargo build --release --jobs $(nproc) > "$LOG_FILE" 2>&1 || error_exit "Falha na compilação"
    increment_step

    # Etapa 7: Instalação dos binários
    show_progress "Instalando binários"
    mv target/release/RustyProxy /opt/rustyproxy/bin/proxy > "$LOG_FILE" 2>&1
    mv /tmp/RustyProxyOnly/menu.sh /opt/rustyproxy/bin/menu > "$LOG_FILE" 2>&1
    ln -sf /opt/rustyproxy/bin/menu /usr/local/bin/rustyproxy > "$LOG_FILE" 2>&1
    increment_step

    # Etapa 8: Configuração de permissões
    show_progress "Configurando permissões de segurança"
    chmod 750 /opt/rustyproxy/bin/* > "$LOG_FILE" 2>&1
    chown -R root:root /opt/rustyproxy > "$LOG_FILE" 2>&1
    setcap CAP_NET_BIND_SERVICE=+eip /opt/rustyproxy/bin/proxy > "$LOG_FILE" 2>&1
    increment_step

    # Etapa 9: Limpeza final
    show_progress "Realizando limpeza final"
    rm -rf /tmp/RustyProxyOnly ~/.cargo/registry > "$LOG_FILE" 2>&1
    increment_step

    echo -e "\n${GREEN}${BOLD}Instalação concluída com sucesso!${NC}"
    echo -e "Execute 'rustyproxy' para acessar o menu de controle\n"
    logger "RustyProxy Installer: Instalação concluída com sucesso"

} | tee -a "$LOG_FILE"

exit 0
