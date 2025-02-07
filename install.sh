#!/bin/bash
# rustyproxy Installer

TOTAL_STEPS=9
CURRENT_STEP=0

# Cores e estilo
GREEN='\033[1;32m'
RED='\033[1;31m'
NC='\033[0m' # Sem cor
BOLD='\033[1m'

show_progress() {
    PERCENT=$((CURRENT_STEP * 100 / TOTAL_STEPS))
    echo -e "${GREEN}${BOLD}PROGRESSO: [${PERCENT}%] - $1${NC}"
}

error_exit() {
    echo -e "\n${RED}${BOLD}ERRO: $1${NC}"
    exit 1
}

increment_step() {
    CURRENT_STEP=$((CURRENT_STEP + 1))
}

if [ "$EUID" -ne 0 ]; then
    error_exit "EXECUTE COMO ROOT"
else
    clear
    show_progress "ATUALIZANDO REPOSITORIOS..."
    export DEBIAN_FRONTEND=noninteractive
    apt update -y > /dev/null 2>&1 || error_exit "FALHA AO ATUALIZAR OS REPOSITORIOS"
    increment_step

    # ---->>>> Verificação do sistema
    show_progress "VERIFICANDO O SISTEMA..."
    if ! command -v lsb_release &> /dev/null; then
        apt install lsb-release -y > /dev/null 2>&1 || error_exit "FALHA AO INSTALAR LSB-RELEASE"
    fi
    increment_step

    # ---->>>> Verificação do sistema
    OS_NAME=$(lsb_release -is)
    VERSION=$(lsb_release -rs)

    case $OS_NAME in
        Ubuntu)
            case $VERSION in
                24.*|22.*|20.*|18.*)
                    show_progress "SISTEMA UBUNTU SUPORTADO, CONTINUANDO..."
                    ;;
                *)
                    error_exit "VERSÃO DO UBUNTU NÃO SUPORTADA. USE 18, 20, 22 OU 24."
                    ;;
            esac
            ;;
        Debian)
            case $VERSION in
                12*|11*|10*|9*)
                    show_progress "SISTEMA DEBIAN SUPORTADO, CONTINUANDO..."
                    ;;
                *)
                    error_exit "VERSÃO DO DEBIAN NÃO SUPORTADA. USE 9, 10, 11 OU 12."
                    ;;
            esac
            ;;
        *)
            error_exit "SISTEMA NÃO SUPORTADO. USE UBUNTU OU DEBIAN."
            ;;
    esac
    increment_step

    # ---->>>> Instalação de pacotes requisitos e atualização do sistema
    show_progress "ATUALIZANDO O SISTEMA..."
    apt update -y > /dev/null 2>&1 || error_exit "FALHA AO ATUALIZAR O SISTEMA"
    apt-get install curl build-essential git -y > /dev/null 2>&1 || error_exit "FALHA AO INSTALAR PACOTES"
    increment_step

    # ---->>>> Criando o diretório do script
    show_progress "CRIANDO DIRETORIO /OPT/RUSTYPROXY..."
    mkdir -p /opt/rustyproxy > /dev/null 2>&1
    increment_step

    # ---->>>> Instalar rust
    show_progress "INSTALANDO RUST..."
    if ! command -v rustc &> /dev/null; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y > /dev/null 2>&1 || error_exit "FALHA AO INSTALAR RUST"
        export PATH="$HOME/.cargo/bin:$PATH"
    fi
    increment_step

    # ---->>>> Instalar o RustyProxy
    show_progress "COMPILANDO RUSTYPROXY, ISSO PODE LEVAR ALGUM TEMPO DEPENDENDO DA MAQUINA..."

    if [ -d "/root/RustyProxyOnly" ]; then
        rm -rf /root/RustyProxyOnly
    fi

    
    git clone https://github.com/PhoenixxZ2023/RustyProxyOnly.git /root/RustyProxyOnly > /dev/null 2>&1 || error_exit "FALHA AO CLONAR RUSTYPROXY"
    mv /root/RustyProxyOnly/menu.sh /opt/rustyproxy/menu
    cd /root/RustyProxyOnly/RustyProxy
    cargo build --release --jobs $(nproc) > /dev/null 2>&1 || error_exit "FALHA AO COMPILAR RUSTYPROXY"
    mv ./target/release/RustyProxy /opt/rustyproxy/proxy
    increment_step

    # ---->>>> Configuração de permissões
    show_progress "CONFIGURANDO PERMISSÕES..."
    chmod +x /opt/rustyproxy/proxy
    chmod +x /opt/rustyproxy/menu
    ln -sf /opt/rustyproxy/menu /usr/local/bin/rustyproxy
    increment_step

    # ---->>>> Limpeza
    show_progress "LIMPANDO DIRETÓRIOS TEMPORÁRIOS..."
    cd /root/
    rm -rf /root/RustyProxyOnly/
    increment_step

    # ---->>>> Instalação finalizada :)
    echo -e "${GREEN}${BOLD}INSTALAÇÃO CONCLUÍDA COM SUCESSO. DIGITE 'rustyproxy' PARA ACESSAR O MENU.${NC}"
fi
