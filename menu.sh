#!/bin/bash

# Configurações
PORTS_FILE="/opt/rustyproxy/ports"
SERVICE_DIR="/etc/systemd/system/rustyproxy"
BIN_PATH="/opt/rustyproxy/proxy"

# Cores
BOLD="\033[1m"
BLUE="\033[34m"
CYAN="\033[36m"
GREEN="\033[32m"
RED="\033[31m"
YELLOW="\033[33m"
RESET="\033[0m"

# Verificar root
verificar_usuario_root() {
    if [ "$(id -u)" -ne 0 ]; then
        echo -e "${RED}${BOLD}O SCRIPT DEVE SER EXECUTADO COMO ROOT. UTILIZE 'SUDO' PARA EXECUTAR O SCRIPT.${RESET}"
        exit 1
    fi
}

# Verificar dependências
check_dependencies() {
    local missing=()
    command -v systemctl >/dev/null || missing+=("systemctl")
    command -v ss >/dev/null || command -v netstat >/dev/null || missing+=("ss/netstat")
    
    if [ ${#missing[@]} -gt 0 ]; then
        echo -e "${RED}${BOLD}Erro: Dependências ausentes: ${missing[*]}${RESET}"
        exit 1
    fi
}

# Verificar porta em uso
is_port_in_use() {
    local port=$1
    if command -v ss &>/dev/null; then
        ss -tuln | grep -q ":${port} "
    else
        netstat -tuln | grep -q ":${port} "
    fi
}

# Adicionar porta
add_proxy_port() {
    local port=$1
    local status=${2:-"@RustyProxy"}
    
    if ! [[ "$port" =~ ^[0-9]+$ ]] || (( port < 1 || port > 65535 )); then
        echo -e "${RED}${BOLD}PORTA INVÁLIDA!${RESET}"
        return 1
    fi

    if grep -q "^${port}$" "$PORTS_FILE"; then
        echo -e "${RED}${BOLD}PORTA JÁ ESTÁ EM USO!${RESET}"
        return 1
    fi

    if is_port_in_use "$port"; then
        echo -e "${RED}${BOLD}PORTA JÁ ESTÁ EM USO POR OUTRO SERVIÇO!${RESET}"
        return 1
    fi

    local service_file="${SERVICE_DIR}/rustyproxy-proxy${port}.service"
    sudo mkdir -p "$SERVICE_DIR"
    
    cat << EOF | sudo tee "$service_file" >/dev/null
[Unit]
Description=RustyProxy${port}
After=network.target

[Service]
Type=simple
ExecStart=${BIN_PATH} --port ${port} --status "${status}"
Restart=always
RestartSec=5
LimitNOFILE=65535
SyslogIdentifier=rustyproxy-proxy${port}

[Install]
WantedBy=multi-user.target
EOF

    sudo systemctl daemon-reload
    sudo systemctl enable "rustyproxy-proxy${port}.service"
    sudo systemctl start "rustyproxy-proxy${port}.service"
    
    if systemctl is-active --quiet "rustyproxy-proxy${port}.service"; then
        echo "$port" | sudo tee -a "$PORTS_FILE" >/dev/null
        echo -e "${GREEN}${BOLD}PORTA $port ABERTA COM SUCESSO.${RESET}"
    else
        echo -e "${RED}${BOLD}FALHA AO INICIAR SERVIÇO!${RESET}"
        sudo rm -f "$service_file"
        return 1
    fi
}

# Remover porta
del_proxy_port() {
    local port=$1
    
    if ! [[ "$port" =~ ^[0-9]+$ ]]; then
        echo -e "${RED}${BOLD}PORTA INVÁLIDA!${RESET}"
        return 1
    fi

    local service="rustyproxy-proxy${port}.service"
    
    if [ ! -f "${SERVICE_DIR}/${service}" ]; then
        echo -e "${RED}${BOLD}SERVIÇO NÃO ENCONTRADO!${RESET}"
        return 1
    fi

    sudo systemctl stop "$service"
    sudo systemctl disable "$service"
    sudo rm -f "${SERVICE_DIR}/${service}"
    sudo systemctl daemon-reload
    sudo sed -i "/^${port}$/d" "$PORTS_FILE"
    
    echo -e "${GREEN}${BOLD}PORTA $port FECHADA COM SUCESSO.${RESET}"
}

# Menu principal
show_menu() {
    clear
    echo -e "\033[1;36m┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓\033[0m"
    echo -e "\033[1;36m┃\033[44;1;37m              MULTI-PROXY              \033[0m\033[1;36m┃"
    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫\033[0m"
    echo -e "\033[1;36m┃\033[1;33mGERENCIAMENTO DE PORTAS - MULTI-PROXY  \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫\033[0m"

    if [ -s "$PORTS_FILE" ]; then
        active_ports=$(tr '\n' ' ' < "$PORTS_FILE")
        echo -e "\033[1;36m┃\033[1;33mPORTAS ATIVAS:\033[1;33m $(printf '%-21s' "$active_ports")   \033[1;36m┃\033[0m"
    fi

    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m01\033[1;31m] \033[1;37m• \033[1;33mABRIR PORTA                     \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m02\033[1;31m] \033[1;37m• \033[1;33mFECHAR PORTA                    \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m00\033[1;31m] \033[1;37m• \033[1;33mSAIR DO MENU                    \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫\033[0m"
    echo -e "\033[1;36m┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛\033[0m"
    read -p "┗━➤ SELECIONE UMA OPÇÃO: " option

    case $option in
        1)
            read -p "━➤ DIGITE A PORTA: " port
            while ! [[ $port =~ ^[0-9]+$ ]]; do
                echo -e "${RED}━➤ DIGITE UMA PORTA VÁLIDA.${RESET}"
                read -p "━➤ DIGITE A PORTA: " port
            done
            read -p "━➤ DIGITE UM STATUS DE CONEXÃO (deixe vazio para o padrão): " status
            add_proxy_port $port "$status"
            read -p "━➤ PRESSIONE ENTER PARA CONTINUAR..." dummy
            ;;
        2)
            read -p "━➤ DIGITE A PORTA: " port
            while ! [[ $port =~ ^[0-9]+$ ]]; do
                echo -e "${RED}━➤ DIGITE UMA PORTA VÁLIDA.${RESET}"
                read -p "━➤ DIGITE A PORTA: " port
            done
            del_proxy_port $port
            read -p "━➤ PRESSIONE ENTER PARA CONTINUAR..." dummy
            ;;
        0)
            exit 0
            ;;
        *)
            echo -e "${RED}OPÇÃO INVÁLIDA!${RESET}"
            read -p "PRESSIONE ENTER PARA CONTINUAR..." dummy
            ;;
    esac
}

# Inicialização
verificar_usuario_root
check_dependencies

# Criar estrutura inicial
sudo mkdir -p "$(dirname "$PORTS_FILE")"
sudo touch "$PORTS_FILE"
sudo chmod 777 "$PORTS_FILE"

# Loop do menu
while true; do
    show_menu
done
