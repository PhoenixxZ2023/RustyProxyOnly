#!/bin/bash

PORTS_FILE="/opt/rustyproxy/ports"

# Cores
BOLD="\033[1m"
BLUE="\033[34m"
CYAN="\033[36m"
GREEN="\033[32m"
RED="\033[31m"
YELLOW="\033[33m"
RESET="\033[0m"

# Função para verificar se o script está sendo executado como root
verificar_usuario_root() {
    if [ "$(id -u)" -ne 0 ]; then
        echo -e "${RED}${BOLD}O SCRIPT DEVE SER EXECUTADO COMO ROOT. UTILIZE 'SUDO' PARA EXECUTAR O SCRIPT.${RESET}"
        exit 1
    fi
}

# Função para verificar se uma porta está em uso
is_port_in_use() {
    local port=$1
    if lsof -i :$port > /dev/null 2>&1; then
        return 0
    else
        return 1
    fi
}

# Função para abrir uma porta de proxy
add_proxy_port() {
    local port=$1
    local status=${2:-"@RustyProxy"}

    if is_port_in_use $port; then
        echo -e "${RED}${BOLD}A PORTA $port JÁ ESTÁ EM USO.${RESET}"
        return
    fi

    local command="/opt/rustyproxy/proxy --port $port --status $status"
    local service_file_path="/etc/systemd/system/rustyproxy-proxy${port}.service"
    local service_file_content="[Unit]
Description=RustyProxy${port}
After=network.target

[Service]
LimitNOFILE=infinity
LimitNPROC=infinity
LimitMEMLOCK=infinity
LimitSTACK=infinity
LimitCORE=infinity
LimitAS=infinity
LimitRSS=infinity
LimitCPU=infinity
LimitFSIZE=infinity
Type=simple
ExecStart=${command}
Restart=always
StandardOutput=syslog
StandardError=syslog
SyslogIdentifier=rustyproxy-proxy${port}

[Install]
WantedBy=multi-user.target"

    sudo mkdir -p /etc/systemd/system/rustyproxy
    echo "$service_file_content" | sudo tee "$service_file_path" > /dev/null
    sudo systemctl daemon-reload
    sudo systemctl enable "rustyproxy-proxy${port}.service"
    sudo systemctl start "rustyproxy-proxy${port}.service"
    
    if systemctl is-active --quiet "rustyproxy-proxy${port}.service"; then
        echo -e "${GREEN}${BOLD}PORTA $port ABERTA COM SUCESSO.${RESET}"
        echo $port | sudo tee -a "$PORTS_FILE" > /dev/null
    else
        echo -e "${RED}${BOLD}FALHA AO ABRIR A PORTA $port. VERIFIQUE OS LOGS DO SISTEMA PARA MAIS DETALHES.${RESET}"
    fi
}

# Função para fechar uma porta de proxy
del_proxy_port() {
    local port=$1

    sudo systemctl disable "rustyproxy-proxy${port}.service"
    sudo systemctl stop "rustyproxy-proxy${port}.service"
    sudo rm -f "/etc/systemd/system/rustyproxy-proxy${port}.service"
    sudo systemctl daemon-reload

    sed -i "/^$port$/d" "$PORTS_FILE"
    echo -e "${GREEN}${BOLD}PORTA $port FECHADA COM SUCESSO.${RESET}"
}

# Função para exibir o menu formatado
show_menu() {
    clear
    echo -e "\033[1;36m┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓\033[0m"
    echo -e "\033[1;36m┃\E[44;1;37m              MULTI-PROXY              \E[0m\033[0;36m┃"
    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫\033[0m"
    echo -e "\033[1;36m┃\033[1;33mGERENCIAMENTO DE PORTAS - MULTI-PROXY  \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫\033[0m"

    # Exibe portas ativas, se houver
    if [ -s "$PORTS_FILE" ]; then
        active_ports=$(paste -sd ' ' "$PORTS_FILE")
        echo -e "\033[1;36m┃\033[1;33mPORTAS ATIVAS:\033[1;33m $(printf '%-21s' "$active_ports")   \033[1;36m┃\033[0m"
    fi

    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m01\033[1;31m] \033[1;37m• \033[1;33mABRIR PORTA                     \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m02\033[1;31m] \033[1;37m• \033[1;33mFECHAR PORTA                    \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m00\033[1;31m] \033[1;37m• \033[1;33mSAIR DO MENU                    \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫\033[0m"
    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛\033[0m"
    read -p "┗━➤ SELECIONE UMA OPÇÃO: " option

    case $option in
        1)
            read -p "━➤ DIGITE A PORTA: " port
            while ! [[ $port =~ ^[0-9]+$ ]]; do
                echo "━➤ DIGITE UMA PORTA VÁLIDA."
                read -p "━➤ DIGITE A PORTA: " port
            done
            read -p "━➤ DIGITE UM STATUS DE CONEXÃO (deixe vazio para o padrão): " status
            add_proxy_port $port "$status"
            read -p "━➤ PORTA ATIVADA COM SUCESSO. PRESSIONE QUALQUER TECLA." dummy
            ;;
        2)
            read -p "━➤ DIGITE A PORTA: " port
            while ! [[ $port =~ ^[0-9]+$ ]]; do
                echo "━➤ DIGITE UMA PORTA VÁLIDA."
                read -p "━➤ DIGITE A PORTA: " port
            done
            del_proxy_port $port
            read -p "━➤ PORTA DESATIVADA COM SUCESSO. PRESSIONE QUALQUER TECLA." dummy
            ;;
        0)
            exit 0
            ;;
        *)
            echo "OPÇÃO INVÁLIDA. PRESSIONE QUALQUER TECLA PARA RETORNAR AO MENU."
            read -n 1 dummy
            ;;
    esac
}

# Verificar se o script está sendo executado como root
verificar_usuario_root

# Verificar se o arquivo de portas existe, caso contrário, criar e definir permissões
if [ ! -f "$PORTS_FILE" ]; then
    sudo touch "$PORTS_FILE"
    sudo chmod 777 "$PORTS_FILE"
fi

# Loop do menu
while true; do
    show_menu
done
