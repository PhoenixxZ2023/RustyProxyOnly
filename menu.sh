#!/bin/bash
# RUSTYPROXY MANAGER

PORTS_FILE="/opt/rustyproxy/ports"

RED="\033[1;31m"
GREEN="\033[1;32m"
YELLOW="\033[1;33m"
BLUE="\033[0;34m"
WHITE_BG="\033[40;1;37m"
RESET="\033[0m"

if [ "$EUID" -ne 0 ]; then
  echo -e "${RED}Por favor, execute este script como root ou com sudo.${RESET}"
  exit 1
fi

add_proxy_port() {
    local port=$1
    local status=${2:-"RUSTY PROXY"}

    if is_port_in_use "$port"; then
        echo -e "${RED}⛔️ A PORTA $port JÁ ESTÁ EM USO.${RESET}"
        return
    fi

    local command="/opt/rustyproxy/proxy --port $port --status \"$status\""
    local service_file_path="/etc/systemd/system/proxy${port}.service"
    local service_file_content="[Unit]
Description=RustyProxy ${port}
After=network.target

[Service]
LimitNOFILE=infinity
Type=simple
ExecStart=${command}
Restart=always

[Install]
WantedBy=multi-user.target"

    echo "$service_file_content" > "$service_file_path"
    systemctl daemon-reload
    systemctl enable "proxy${port}.service"
    systemctl start "proxy${port}.service"

    echo "$port" >> "$PORTS_FILE"
    echo -e "${GREEN}✅ PORTA $port ABERTA COM SUCESSO.${RESET}"
}

is_port_in_use() {
    local port=$1
    if netstat -tuln 2>/dev/null | awk '{print $4}' | grep -q ":$port$"; then
        return 0
    elif ss -tuln 2>/dev/null | awk '{print $4}' | grep -q ":$port$"; then
        return 0
    elif lsof -i :"$port" 2>/dev/null | grep -q LISTEN; then
        return 0
    else
        return 1
    fi
}

del_proxy_port() {
    local port=$1

    systemctl disable "proxy${port}.service"
    systemctl stop "proxy${port}.service"
    rm -f "/etc/systemd/system/proxy${port}.service"
    systemctl daemon-reload

    if lsof -i :"$port" &>/dev/null; then
        fuser -k "$port"/tcp 2>/dev/null
    fi

    sed -i "/^$port|/d" "$PORTS_FILE"
    echo -e "${GREEN}✅ PORTA $port FECHADA COM SUCESSO.${RESET}"
}

update_proxy_status() {
    local port=$1
    local new_status=$2
    local service_file_path="/etc/systemd/system/proxy${port}.service"

    if ! is_port_in_use "$port"; then
        echo -e "${YELLOW}⚠️ A PORTA $port NÃO ESTÁ ATIVA.${RESET}"
        return
    fi

    if [ ! -f "$service_file_path" ]; then
        echo -e "${RED}ARQUIVO DE SERVIÇO PARA $port NÃO ENCONTRADO.${RESET}"
        return
    fi

    local new_command="/opt/rustyproxy/proxy --port $port --status \"$new_status\""
    sed -i "s|^ExecStart=.*$|ExecStart=${new_command}|" "$service_file_path"

    systemctl daemon-reload
    systemctl restart "proxy${port}.service"

    sed -i "s/^$port|.*/$port|$new_status/" "$PORTS_FILE"

    echo -e "${YELLOW}🔃 STATUS DA PORTA $port ATUALIZADO PARA '$new_status'.${RESET}"
    sleep 2
}

uninstall_rustyproxy() {
    echo -e "${YELLOW}🗑️ DESINSTALANDO RUSTY PROXY, AGUARDE...${RESET}"
    sleep 2
    clear

    if [ -s "$PORTS_FILE" ]; then
        while IFS='|' read -r port _; do
            del_proxy_port "$port"
        done < "$PORTS_FILE"
    fi

    rm -rf /opt/rustyproxy
    rm -f "$PORTS_FILE"

    echo -e "\033[0;36m┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓\033[0m"
    echo -e "\033[1;36m┃\E[44;1;37m RUSTY PROXY DESINSTALADO COM SUCESSO. \E[0m\033[0;36m┃"
    echo -e "\033[0;36m┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛\033[0m"
    sleep 3
    clear
}

restart_all_proxies() {
    if [ ! -s "$PORTS_FILE" ]; then
        echo "NENHUMA PORTA ENCONTRADA PARA REINICIAR."
        return
    fi

    echo "🔃 REINICIANDO TODAS AS PORTAS DO PROXY..."
    sleep 2

    while IFS='|' read -r port status; do
        del_proxy_port "$port"
        add_proxy_port "$port" "$status"
    done < "$PORTS_FILE"

    echo -e "${GREEN}✅ TODAS AS PORTAS FORAM REINICIADAS COM SUCESSO.${RESET}"
    sleep 2
}

# Função para exibir o menu formatado
show_menu() {
    clear
    echo -e "\033[1;36m┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓\033[0m"
    echo -e "\033[1;36m┃\E[44;1;37m              MULTI-PROXY              \E[0m\033[0;36m┃"
    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫\033[0m"
    echo -e "\033[1;36m┃\033[1;33mGERENCIAMENTO DE PORTAS - MULTI-PROXY  \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫\033[0m"

    if [ -s "$PORTS_FILE" ]; then
        active_ports=$(paste -sd ' ' "$PORTS_FILE")
        echo -e "\033[1;36m┃\033[1;33mPORTAS ATIVAS:\033[1;33m $(printf '%-21s' "$active_ports")   \033[1;36m┃\033[0m"
    fi

    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m01\033[1;31m] \033[1;37m◉ \033[1;33mATIVAR PROXY                    \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m02\033[1;31m] \033[1;37m◉ \033[1;33mDESATIVAR PROXY                 \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m03\033[1;31m] \033[1;37m◉ \033[1;33mREINICIAR PROXY                 \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m04\033[1;31m] \033[1;37m◉ \033[1;33mALTERAR STATUS                  \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m05\033[1;31m] \033[1;37m◉ \033[1;33mDESINTALAR SCRIPT               \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m00\033[1;31m] \033[1;37m◉ \033[1;33mSAIR DO MENU                    \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫\033[0m"
    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛\033[0m"
    read -p "┗━➤ SELECIONE UMA OPÇÃO: " option

    case $option in
        1)
	    clear
            read -p "━➤ DIGITE A PORTA: " port
            while ! [[ $port =~ ^[0-9]+$ ]]; do
                echo "━➤ DIGITE UMA PORTA VÁLIDA."
                read -p "━➤ DIGITE A PORTA: " port
            done
            read -p "━➤ DIGITE UM STATUS DE CONEXÃO (deixe vazio para o padrão): " status
            add_proxy_port $port 
            read -p "━➤ PORTA ATIVADA COM SUCESSO. PRESSIONE QUALQUER TECLA." dummy
            ;;
        2)
            clear
            read -p "DIGITE A PORTA: " port
            while ! [[ $port =~ ^[0-9]+$ ]]; do
                echo "━➤ DIGITE UMA PORTA VÁLIDA."
                read -p "━➤ DIGITE A PORTA: " port
            done
            del_proxy_port "$port"
            sleep 3
            clear
            read -p "━➤ PORTA DESATIVADA COM SUCESSO. PRESSIONE QUALQUER TECLA." dummy
            ;;
        3)
            clear
            restart_all_proxies
            sleep 3
            clear
            read -n 1 -s -r -p "━➤ PRESSIONE QUALQUER TECLA PARA VOLTAR AO MENU."
            ;;
	4)
            clear
            read -p "━➤ DIGITE A PORTA: " port
            while ! [[ $port =~ ^[0-9]+$ ]]; do
                echo "━➤ DIGITE UMA PORTA VÁLIDA."
                read -p "━➤ DIGITE A PORTA: " port
            done
            read -p "━➤ DIGITE O NOVO STATUS DO PROXY: " new_status
            update_proxy_status "$port" "$new_status"
            sleep 3
            clear
            read -n 1 -s -r -p "━➤ PRESSIONE QUALQUER TECLA PARA VOLTAR AO MENU."
            ;;
        5)
            clear
            uninstall_rustyproxy
            sleep 3
            clear
            read -n 1 -s -r -p "━➤ PRESSIONE QUALQUER TECLA PARA SAIR."
            clear
            menu
            ;;
        0)
	    clear
            exit 0
            ;;
        *)
            echo "OPÇÃO INVÁLIDA. PRESSIONE QUALQUER TECLA PARA VOLTAR AO MENU."
            read -n 1 -s -r
            ;;
    esac
}

[ ! -f "$PORTS_FILE" ] && touch "$PORTS_FILE"

while true; do
    show_menu
done
