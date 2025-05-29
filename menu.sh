#!/bin/bash
# RUSTYPROXY MANAGER

PORTS_FILE="/opt/rustyproxy/ports"
STUNNEL_CONF_DIR="/etc/stunnel"
STUNNEL_SERVICE_FILE="/etc/systemd/system/stunnel_custom.service" # Nome do serviço systemd para o stunnel separado
STUNNEL_CONFIG_FILE="$STUNNEL_CONF_DIR/stunnel_service.conf"
STUNNEL_CERT_FILE="$STUNNEL_CONF_DIR/stunnel_cert.pem"
STUNNEL_KEY_FILE="$STUNNEL_CONF_DIR/key.pem"
STUNNEL_LOG_FILE="/var/log/stunnel4/stunnel_custom.log"
STUNNEL_STATUS_FILE="/opt/stunnel_status.txt" # Novo arquivo para salvar portas do stunnel autônomo

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

# Função auxiliar para validar portas (mantida)
validate_port() {
    local port=$1
    if ! [[ "$port" =~ ^[0-9]+$ ]] || [ "$port" -lt 1 ] || [ "$port" -gt 65535 ]; then
        echo -e "${RED}Porta inválida. Por favor, digite um número entre 1 e 65535.${RESET}"
        return 1
    fi
    return 0
}

# --- Funções originais do RustyProxy (MANTIDAS INALTERADAS) ---
add_proxy_port() {
    local port=$1
    local status=${2:-"RUSTY PROXY"}

    if is_port_in_use "$port"; then
        echo -e "${RED}⛔️ A PORTA $port JÁ ESTÁ EM USO.${RESET}"
        return
    fi

    # ESTA LINHA PERMANECE COMO NO SEU ORIGINAL: APENAS --port E --status
    # Isso significa que o RustyProxy usará as portas de backend padrão (SSH, OpenVPN, WS, Stunnel)
    # que estão hardcoded no main.rs, a menos que você as mude manualmente no main.rs.
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

    systemctl disable "proxy${port}.service" 2>/dev/null
    systemctl stop "proxy${port}.service" 2>/dev/null
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

    # O PORTS_FILE original só guarda a porta, não o status associado
    # Então, para atualizar o status, precisaríamos relê-lo ou ter outra forma de persistência
    # Como o original não guardava status, esta parte é um pouco complexa de manter 100% fiel
    # sem mudar o formato do PORTS_FILE. Por agora, vamos manter o update básico.
    echo -e "${YELLOW}🔃 STATUS DA PORTA $port ATUALIZADO PARA '$new_status'. (Verifique o arquivo de serviço para detalhes).${RESET}"
    sleep 2
}

# restart_all_proxies permanece como no seu original
restart_all_proxies() {
    if [ ! -s "$PORTS_FILE" ]; then
        echo "NENHUMA PORTA ENCONTRADA PARA REINICIAR."
        return
    fi

    echo "🔃 REINICIANDO TODAS AS PORTAS DO PROXY..."
    sleep 2

    # Este loop depende que PORTS_FILE contenha apenas a porta, como no seu original
    while IFS='|' read -r port status; do # O 'status' aqui leria a parte após '|' se existisse
        del_proxy_port "$port" # Desativa e remove o serviço antigo
        # Reativa com o status original (se o PORTS_FILE o tivesse salvo, senão usa padrão)
        add_proxy_port "$port" "$status" # Passa o status, que pode ser vazio
    done < "$PORTS_FILE"

    echo -e "${GREEN}✅ TODAS AS PORTAS FORAM REINICIADAS COM SUCESSO.${RESET}"
    sleep 2
}


# --- NOVAS Funções para o Stunnel Autônomo ---

# Instala o stunnel4
install_stunnel() {
    if ! command -v stunnel4 &> /dev/null; then
        echo -e "${YELLOW}Instalando stunnel4...${NC}"
        apt update > /dev/null 2>&1
        apt install stunnel4 -y > /dev/null 2>&1 || { echo -e "${RED}Erro: Falha ao instalar stunnel4.${NC}"; return 1; }
        echo -e "${GREEN}stunnel4 instalado com sucesso.${NC}"
    else
        echo -e "${GREEN}stunnel4 já está instalado.${NC}"
    fi
    return 0
}

# Cria o certificado para o stunnel (com chaves)
create_stunnel_cert() {
    if [ ! -f "$STUNNEL_CERT_FILE" ]; then
        echo -e "${YELLOW}Gerando certificado SSL/TLS para stunnel...${NC}"
        mkdir -p "$STUNNEL_CONF_DIR" || { echo -e "${RED}Erro: Falha ao criar diretório $STUNNEL_CONF_DIR.${NC}"; return 1; }
        openssl genrsa -out "$STUNNEL_KEY_FILE" 2048 || { echo -e "${RED}Erro: Falha ao gerar chave privada.${NC}"; return 1; }
        # Pode ajustar o CN (Common Name) para o seu domínio real, se tiver um
        openssl req -new -x509 -key "$STUNNEL_KEY_FILE" -out "$STUNNEL_CERT_FILE" -days 365 -nodes \
            -subj "/C=BR/ST=SP/L=SaoPaulo/O=StunnelOrg/OU=IT/CN=your_server_ip_or_domain.com" > /dev/null 2>&1 || { echo -e "${RED}Erro: Falha ao gerar certificado autoassinado. Verifique openssl.${NC}"; return 1; }
        
        # Concatena a chave e o certificado para o arquivo .pem que o stunnel espera
        cat "$STUNNEL_KEY_FILE" "$STUNNEL_CERT_FILE" > "$STUNNEL_CERT_FILE" || { echo -e "${RED}Erro: Falha ao concatenar chave e certificado.${NC}"; return 1; }
        echo -e "${GREEN}Certificado autoassinado gerado em $STUNNEL_CERT_FILE${NC}"
    else
        echo -e "${GREEN}Certificado SSL/TLS já existe em $STUNNEL_CERT_FILE${NC}"
    fi
    return 0
}

# Cria o arquivo de configuração do stunnel
create_stunnel_config() {
    local listen_port=$1
    local connect_host=$2
    local connect_port=$3

    if [ ! -f "$STUNNEL_CERT_FILE" ]; then
        echo -e "${RED}Erro: Certificado SSL/TLS não encontrado em $STUNNEL_CERT_FILE. Gere-o primeiro.${NC}"
        return 1
    fi

    echo -e "${YELLOW}Criando configuração para stunnel na porta ${listen_port}...${NC}"
    mkdir -p /var/log/stunnel4 # Garante que o diretório de log exista
    cat <<EOF > "$STUNNEL_CONFIG_FILE"
foreground = yes
setuid = root
setgid = root
pid = 
debug = 7
output = $STUNNEL_LOG_FILE

[stunnel_proxy]
accept = 0.0.0.0:${listen_port}
connect = ${connect_host}:${connect_port}
cert = ${STUNNEL_CERT_FILE}
client = no
EOF
    echo -e "${GREEN}Configuração do stunnel criada em $STUNNEL_CONFIG_FILE${NC}"
    return 0
}

# Inicia o serviço stunnel autônomo
start_stunnel_standalone_service() {
    local listen_port=$1
    local connect_host=$2
    local connect_port=$3

    install_stunnel || return 1
    create_stunnel_cert || return 1

    # Verifica se a porta já está em uso, mas ignora se for o próprio stunnel custom
    if is_port_in_use "$listen_port"; then
        if systemctl is-active stunnel_custom.service &>/dev/null && grep -q "accept = 0.0.0.0:${listen_port}" "$STUNNEL_CONFIG_FILE" &>/dev/null; then
            echo -e "${YELLOW}Stunnel já está ativo na porta ${listen_port}. Reiniciando para aplicar configurações...${NC}"
            systemctl restart stunnel_custom.service || { echo -e "${RED}Erro ao reiniciar stunnel_custom.service.${NC}"; return 1; }
            echo "$listen_port|$connect_host|$connect_port" > "$STUNNEL_STATUS_FILE" # Salva a config
            echo -e "${GREEN}✅ Stunnel reiniciado com sucesso na porta ${listen_port}.${NC}"
            return 0
        else
            echo -e "${RED}⛔️ A PORTA $listen_port JÁ ESTÁ EM USO por outro serviço.${RESET}"
            return 1
        fi
    fi

    create_stunnel_config "$listen_port" "$connect_host" "$connect_port" || return 1

    # Cria o serviço systemd personalizado para o stunnel
    echo -e "${YELLOW}Criando serviço systemd para stunnel na porta ${listen_port}...${NC}"
    cat <<EOF > "$STUNNEL_SERVICE_FILE"
[Unit]
Description=Stunnel Custom Service on Port ${listen_port}
After=network.target

[Service]
ExecStart=/usr/bin/stunnel4 $STUNNEL_CONFIG_FILE
Restart=always
User=root
Group=root
StandardOutput=syslog
StandardError=syslog
SyslogIdentifier=stunnel_custom_${listen_port}

[Install]
WantedBy=multi-user.target
EOF

    systemctl daemon-reload || { echo -e "${RED}Erro: Falha ao recarregar daemons do systemd.${NC}"; return 1; }
    systemctl enable stunnel_custom.service || { echo -e "${RED}Erro: Falha ao habilitar serviço stunnel_custom.${NC}"; return 1; }
    systemctl start stunnel_custom.service || { echo -e "${RED}Erro: Falha ao iniciar serviço stunnel_custom. Verifique os logs (journalctl -u stunnel_custom.service).${NC}"; return 1; }

    echo "$listen_port|$connect_host|$connect_port" > "$STUNNEL_STATUS_FILE" # Salva a config
    echo -e "${GREEN}✅ Stunnel ativado na porta ${listen_port}, conectando a ${connect_host}:${connect_port}.${NC}"
    return 0
}

# Para o serviço stunnel autônomo
stop_stunnel_standalone_service() {
    echo -e "${YELLOW}Parando serviço stunnel autônomo...${NC}"
    if systemctl is-active stunnel_custom.service &>/dev/null; then
        systemctl stop stunnel_custom.service || { echo -e "${RED}Erro ao parar stunnel_custom.service.${NC}"; return 1; }
        systemctl disable stunnel_custom.service || { echo -e "${RED}Erro ao desabilitar stunnel_custom.service.${NC}"; return 1; }
        echo -e "${GREEN}Stunnel autônomo parado e desabilitado.${NC}"
    else
        echo -e "${YELLOW}Stunnel autônomo não está ativo ou não foi configurado.${NC}"
    fi
    # Limpa o arquivo de status
    if [ -f "$STUNNEL_STATUS_FILE" ]; then
        rm "$STUNNEL_STATUS_FILE"
    fi
    return 0
}

# --- Funções de Desinstalação (Modificada para incluir Stunnel autônomo na desinstalação geral) ---
uninstall_rustyproxy() { # Nome original, mas agora desinstala o Stunnel também
    echo -e "${YELLOW}🗑️ DESINSTALANDO RUSTY PROXY E SERVIÇO STUNNEL (SE ATIVO), AGUARDE...${RESET}"
    sleep 2
    clear

    # Desinstala todos os proxies RustyProxy
    if [ -s "$PORTS_FILE" ]; then
        while IFS='|' read -r port _; do
            del_proxy_port "$port"
        done < "$PORTS_FILE"
    fi

    # Desinstala o stunnel autônomo, se estiver ativo
    stop_stunnel_standalone_service
    if [ -f "$STUNNEL_SERVICE_FILE" ]; then
        rm "$STUNNEL_SERVICE_FILE"
        systemctl daemon-reload
    fi
    if [ -d "$STUNNEL_CONF_DIR" ]; then # Remove a pasta de configuração completa
        rm -rf "$STUNNEL_CONF_DIR"
    fi
    if [ -f "$STUNNEL_STATUS_FILE" ]; then
        rm "$STUNNEL_STATUS_FILE"
    fi
    # Tenta remover o pacote stunnel4 se não for mais necessário
    if dpkg -s stunnel4 &>/dev/null; then
        echo -e "${YELLOW}Removendo pacote stunnel4...${NC}"
        apt autoremove stunnel4 -y > /dev/null 2>&1
    fi

    rm -rf /opt/rustyproxy
    rm -f "$PORTS_FILE"

    echo -e "\033[0;36m┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓\033[0m"
    echo -e "\033[1;36m┃\E[44;1;37m RUSTY PROXY & STUNNEL DESINSTALADOS COM SUCESSO. \E[0m\033[0;36m┃"
    echo -e "\033[0;36m┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛\033[0m"
    sleep 3
    clear
}

# --- Menu Principal (Opções e Estrutura originais mantidas, com novas opções Stunnel) ---
show_menu() {
    clear
    echo -e "\033[1;36m┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓\033[0m"
    echo -e "\033[1;36m┃\E[44;1;37m              MULTI-PROXY              \E[0m\033[0;36m┃"
    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫\033[0m"
    echo -e "\033[1;36m┃\033[1;33mGERENCIAMENTO DE PROXY/STUNNEL        \033[1;36m┃\033[0m" # Título ajustado
    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫\033[0m"

    # Exibição de Portas do RustyProxy (Original)
    if [ -s "$PORTS_FILE" ]; then
        active_proxies_status=$(cat "$PORTS_FILE" | while IFS='|' read -r port; do
            # Tentativa de pegar o status real do serviço, não apenas o do arquivo
            local service_active=$(systemctl is-active proxy${port}.service 2>/dev/null || echo "inactive")
            local active_status_icon=""
            local color_code=""
            if [ "$service_active" = "active" ]; then
                active_status_icon="✅ ATIVO"
                color_code="${GREEN}"
            else
                active_status_icon="❌ INATIVO"
                color_code="${RED}"
            fi
            echo -e " ${color_code} - ${port} (${active_status_icon})${RESET}"
        done)
        echo -e "\033[1;36m┃\033[1;33mPORTAS RUSTYPROXY ATIVAS:${RESET}"
        echo -e "$active_proxies_status"
    else
        echo -e "\033[1;36m┃ ${YELLOW}Nenhuma porta RustyProxy ativa.${RESET}"
    fi
    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫\033[0m"

    # Exibição do Status do Stunnel Autônomo
    echo -e "\033[1;36m┃\033[1;33mSTATUS STUNNEL AUTÔNOMO:${NC}"
    local stunnel_status=$(systemctl is-active stunnel_custom.service 2>/dev/null)
    if [ "$stunnel_status" == "active" ]; then
        if [ -f "$STUNNEL_STATUS_FILE" ]; then
            local stunnel_config=$(cat "$STUNNEL_STATUS_FILE")
            local stunnel_listen_port=$(echo "$stunnel_config" | cut -d'|' -f1)
            local stunnel_connect_host=$(echo "$stunnel_config" | cut -d'|' -f2)
            local stunnel_connect_port=$(echo "$stunnel_config" | cut -d'|' -f3)
            echo -e "\033[1;36m┃ ${GREEN}[+] ATIVO na porta ${stunnel_listen_port} -> ${stunnel_connect_host}:${stunnel_connect_port}${NC}"
        else
            echo -e "\033[1;36m┃ ${YELLOW}[?] Ativo, mas config. desconhecida.${NC}"
        fi
    else
        echo -e "\033[1;36m┃ ${RED}[-] INATIVO.${NC}"
    fi
    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫\033[0m"


    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m01\033[1;31m] \033[1;37m◉ \033[1;33mATIVAR PROXY (RustyProxy)       \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m02\033[1;31m] \033[1;37m◉ \033[1;33mDESATIVAR PROXY (RustyProxy)      \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m03\033[1;31m] \033[1;37m◉ \033[1;33mREINICIAR TODOS PROXYS (RustyProxy) \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m04\033[1;31m] \033[1;37m◉ \033[1;33mALTERAR STATUS (RustyProxy)       \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m05\033[1;31m] \033[1;37m◉ \033[1;33mDESINSTALAR RustyProxy & Stunnel  \033[1;36m┃\033[0m" # Texto ajustado
    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m06\033[1;31m] \033[1;37m◉ \033[1;33mATIVAR/CONFIGURAR Stunnel Autônomo \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m07\033[1;31m] \033[1;37m◉ \033[1;33mDESATIVAR Stunnel Autônomo       \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m08\033[1;31m] \033[1;37m◉ \033[1;33mREINICIAR Stunnel Autônomo       \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m09\033[1;31m] \033[1;37m◉ \033[1;33mVer Logs do Stunnel Autônomo    \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┫\033[0m"
    echo -e "\033[1;36m┃\033[1;31m[\033[1;34m00\033[1;31m] \033[1;37m◉ \033[1;33mSAIR DO MENU                    \033[1;36m┃\033[0m"
    echo -e "\033[1;36m┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛\033[0m"
    read -p "┗━➤ SELECIONE UMA OPÇÃO: " option

    case $option in
        1) # ATIVAR PROXY (RustyProxy)
	    clear
            read -p "━➤ DIGITE A PORTA: " port
            while ! validate_port "$port"; do
                echo "━➤ DIGITE UMA PORTA VÁLIDA."
                read -p "━➤ DIGITE A PORTA: " port
            done
            read -p "━➤ DIGITE UM STATUS DE CONEXÃO (deixe vazio para o padrão): " status
            add_proxy_port $port "$status"
            read -p "━➤ PORTA ATIVADA COM SUCESSO. PRESSIONE QUALQUER TECLA." dummy
            ;;
        2) # DESATIVAR PROXY (RustyProxy)
            clear
            read -p "DIGITE A PORTA PARA DESATIVAR: " port
            while ! validate_port "$port"; do read -p "━➤ DIGITE UMA PORTA VÁLIDA: " port; done
            del_proxy_port "$port"
            sleep 1
            read -p "━➤ PORTA DESATIVADA COM SUCESSO. PRESSIONE QUALQUER TECLA." dummy
            ;;
        3) # REINICIAR PROXY (RustyProxy)
            clear
            restart_all_proxies
            sleep 1
            read -n 1 -s -r -p "━➤ PRESSIONE QUALQUER TECLA PARA VOLTAR AO MENU."
            ;;
	4) # ALTERAR STATUS (RustyProxy)
            clear
            read -p "━➤ DIGITE A PORTA CUJO STATUS DESEJA ALTERAR: " port
            while ! validate_port "$port"; do read -p "━➤ DIGITE UMA PORTA VÁLIDA: " port; done
            read -p "━➤ DIGITE O NOVO STATUS DO PROXY: " new_status
            new_status=${new_status:-"RUSTY PROXY"}
            update_proxy_status "$port" "$new_status"
            sleep 1
            read -n 1 -s -r -p "━➤ PRESSIONE QUALQUER TECLA PARA VOLTAR AO MENU."
            ;;
        5) # DESINSTALAR RustyProxy & Stunnel (Texto ajustado)
            clear
            uninstall_rustyproxy
            sleep 1
            exit 0
            ;;
        # --- NOVAS OPÇÕES PARA STUNNEL AUTÔNOMO ---
        6) # ATIVAR/CONFIGURAR Stunnel Autônomo
            clear
            echo -e "${YELLOW}Configurar e Ativar Stunnel Autônomo${NC}"
            echo " "
            read -p "━➤ Digite a PORTA DE ESCUTA para o Stunnel (sugestão: 443 ou 8443): " stunnel_listen_port
            stunnel_listen_port=${stunnel_listen_port:-443} # Padrão sugerido
            while ! validate_port "$stunnel_listen_port"; do read -p "━➤ Digite uma porta válida para o Stunnel: " stunnel_listen_port; done

            read -p "━➤ Digite o HOST DE DESTINO para o Stunnel (ex: 127.0.0.1 para serviço local, ou IP/domínio remoto): " stunnel_connect_host
            if [ -z "$stunnel_connect_host" ]; then
                echo -e "${RED}Host de destino não pode ser vazio.${RESET}"
                sleep 2
                continue
            fi

            read -p "━➤ Digite a PORTA DE DESTINO para o Stunnel (padrão: 22 para SSH, 1194 para OpenVPN, 80 para web server): " stunnel_connect_port
            stunnel_connect_port=${stunnel_connect_port:-22}
            while ! validate_port "$stunnel_connect_port"; do read -p "━➤ Digite uma porta de destino válida para o Stunnel: " stunnel_connect_port; done
            
            start_stunnel_standalone_service "$stunnel_listen_port" "$stunnel_connect_host" "$stunnel_connect_port"
            read -p "━➤ Operação do Stunnel concluída. Pressione qualquer tecla." dummy
            ;;
        7) # DESATIVAR Stunnel Autônomo
            clear
            stop_stunnel_standalone_service
            sleep 1
            read -p "━➤ Operação do Stunnel concluída. Pressione qualquer tecla." dummy
            ;;
        8) # REINICIAR Stunnel Autônomo
            clear
            echo -e "${YELLOW}Reiniciando Stunnel Autônomo...${NC}"
            if systemctl is-active stunnel_custom.service &>/dev/null && [ -f "$STUNNEL_STATUS_FILE" ]; then
                local stunnel_config=$(cat "$STUNNEL_STATUS_FILE")
                local stunnel_listen_port=$(echo "$stunnel_config" | cut -d'|' -f1)
                local stunnel_connect_host=$(echo "$stunnel_config" | cut -d'|' -f2)
                local stunnel_connect_port=$(echo "$stunnel_config" | cut -d'|' -f3)

                stop_stunnel_standalone_service # Para para garantir que tudo está limpo
                start_stunnel_standalone_service "$stunnel_listen_port" "$stunnel_connect_host" "$stunnel_connect_port" # Inicia novamente
                echo -e "${GREEN}Stunnel autônomo reiniciado com sucesso!${NC}"
            else
                echo -e "${RED}Stunnel autônomo não está ativo para reiniciar. Ative-o primeiro (Opção 6).${NC}"
            fi
            sleep 2
            read -p "━➤ Operação do Stunnel concluída. Pressione qualquer tecla." dummy
            ;;
        9) # Ver Logs do Stunnel Autônomo
            clear
            echo -e "${YELLOW}Exibindo logs do Stunnel Autônomo (pressione Ctrl+C para sair)...${NC}"
            journalctl -u stunnel_custom.service -f
            ;;
        0) # SAIR
	    clear
            exit 0
            ;;
        *) # OPÇÃO INVÁLIDA
            echo -e "${RED}OPÇÃO INVÁLIDA. PRESSIONE QUALQUER TECLA PARA VOLTAR AO MENU.${RESET}"
            read -n 1 -s -r
            ;;
    esac
done

[ ! -f "$PORTS_FILE" ] && touch "$PORTS_FILE" # Garante que o arquivo de portas do RustyProxy exista
[ ! -f "$STUNNEL_STATUS_FILE" ] && touch "$STUNNEL_STATUS_FILE" # Garante que o arquivo de status do Stunnel exista

while true; do
    show_menu
done
