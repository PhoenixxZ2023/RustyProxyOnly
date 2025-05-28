#!/bin/bash
# Instalação Rusty Proxy

TOTAL_STEPS=9
CURRENT_STEP=0

show_progress() {
    PERCENT=$((CURRENT_STEP * 100 / TOTAL_STEPS))
    echo "Progresso: [${PERCENT}%] - $1"
}

error_exit() {
    echo -e "\nErro: $1"
    exit 1
}

increment_step() {
    CURRENT_STEP=$((CURRENT_STEP + 1))
}

if [ "$EUID" -ne 0 ]; then
    error_exit "EXECUTE COMO ROOT"
else
    clear
    echo ""
echo -e "\033[0;34m           ╦═╗╦ ╦╔═╗╔╦╗╦ ╦  ╔═╗╦═╗╔═╗═╗ ╦╦ ╦                    "
echo -e "\033[0;37m           ╠╦╝║ ║╚═╗ ║ ╚╦╝  ╠═╝╠╦╝║ ║╔╩╦╝╚╦╝                    "
echo -e "\033[0;34m           ╩╚═╚═╝╚═╝ ╩  ╩   ╩  ╩╚═╚═╝╩ ╚═ ╩  \033[0;37m2025        "
    echo -e " "             
    echo -e " "
    show_progress "ATUALIZANDO REPOSITÓRIO..."
    export DEBIAN_FRONTEND=noninteractive
    apt update -y > /dev/null 2>&1 || error_exit "Falha ao atualizar os repositorios"
    increment_step

    # ---->>>> Verificação do sistema
    show_progress "VERIFICANDO SISTEMA..."
    if ! command -v lsb_release &> /dev/null; then
        apt install lsb-release -y > /dev/null 2>&1 || error_exit "Falha ao instalar lsb-release"
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
                    error_exit "VERSÃO DO UBUNTU NÃO SUPORTADA. USE UBUNTU 18, 20, 22 ou 24."
                    ;;
            esac
            ;;
        Debian)
            case $VERSION in
                12*|11*|10*|9*)
                    show_progress "SISTEMA DEBIAN SUPORTADO, CONTINUANDO..."
                    ;;
                *)
                    error_exit "VERSÃO DO DEBIAN NÃO SUPORTADA. USE DEBIAN 9, 10, 11 ou 12."
                    ;;
            esac
            ;;
        *)
            error_exit "SISTEMA NÃO SUPORTADO. USE UBUNTU OU DEBIAN."
            ;;
    esac
    increment_step

    # ---->>>> Instalação de pacotes requisitos e atualização do sistema
    show_progress "ATUALIZANDO O SISTEMA E INSTALANDO DEPENDÊNCIAS, AGUARDE..."
    apt upgrade -y > /dev/null 2>&1 || error_exit "Falha ao atualizar o sistema"
    apt-get install curl build-essential git -y > /dev/null 2>&1 || error_exit "Falha ao instalar pacotes essenciais (curl, build-essential, git)"
    increment_step

    # ---->>>> Criando o diretório do script
    show_progress "CRIANDO DIRETÓRIO PARA O PROXY..."
    mkdir -p /opt/rustyproxy > /dev/null 2>&1 || error_exit "Falha ao criar o diretório /opt/rustyproxy"
    increment_step

    # ---->>>> Instalar rust
    show_progress "INSTALANDO RUST TOOLCHAIN, ISSO PODE LEVAR ALGUNS MINUTOS..."
    if ! command -v rustc &> /dev/null; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y > /dev/null 2>&1 || error_exit "Falha ao instalar Rust"
        # Importante: Source o ambiente do Cargo para que `cargo build` funcione no script
        source "/root/.cargo/env" || error_exit "Falha ao carregar o ambiente Rust. Verifique a instalação do Rust."
    else
        echo "Rust já está instalado."
    fi
    increment_step

    # ---->>>> Instalar o RustyProxy
    show_progress "CLONANDO E COMPILANDO RUSTYPROXY, ISSO PODE LEVAR UM TEMPO, AGUARDE..."

    # Garante que o diretório de clone está limpo
    if [ -d "/root/RustyProxyOnly" ]; then
        rm -rf /root/RustyProxyOnly || error_exit "Falha ao remover diretório antigo de clone"
    fi

    # Clona o repositório
    git clone --branch "main" https://github.com/PhoenixxZ2023/RustyProxyOnly.git /root/RustyProxyOnly > /dev/null 2>&1 || error_exit "Falha ao clonar o repositório RustyProxyOnly"
    
    # Move o menu.sh para o diretório de instalação
    mv /root/RustyProxyOnly/menu.sh /opt/rustyproxy/menu || error_exit "Falha ao mover o script de menu"
    
    # Navega para o diretório do projeto Rust e compila
    cd /root/RustyProxyOnly/RustyProxy || error_exit "Diretório do projeto Rust não encontrado"
    
    # *** LINHA MODIFICADA AQUI ***
    # Remove o redirecionamento para ver os erros de compilação
    cargo build --release --jobs $(nproc) || error_exit "Falha ao compilar o RustyProxy. Verifique o output acima para detalhes."
    
    # Move o executável compilado
    mv ./target/release/RustyProxy /opt/rustyproxy/proxy || error_exit "Falha ao mover o executável compilado"
    increment_step

    # ---->>>> Configuração de permissões
    show_progress "CONFIGURANDO PERMISSÕES E CRIANDO LINK SIMBÓLICO..."
    chmod +x /opt/rustyproxy/proxy || error_exit "Falha ao definir permissões para o proxy"
    chmod +x /opt/rustyproxy/menu || error_exit "Falha ao definir permissões para o menu"
    ln -sf /opt/rustyproxy/menu /usr/local/bin/rustyproxy || error_exit "Falha ao criar link simbólico para o menu"
    increment_step

    # ---->>>> Limpeza
    show_progress "LIMPANDO DIRETÓRIOS TEMPORÁRIOS, AGUARDE..."
    cd /root/ || error_exit "Não foi possível retornar ao diretório /root/"
    rm -rf /root/RustyProxyOnly/ || error_exit "Falha ao limpar diretórios temporários"
    increment_step

    # ---->>>> Instalação finalizada :)
clear
echo -e " "
echo -e "\033[0;34m--------------------------------------------------------------\033[0m"
echo -e "\E[44;1;37m        INSTALAÇÃO FINALIZADA COM SUCESSO               \E[0m"
echo -e "\033[0;34m--------------------------------------------------------------\033[0m"
echo -e " "
echo -e "\033[1;31m \033[1;33mDIGITE O COMANDO PARA ACESSAR O MENU: \033[1;32mrustyproxy\033[0m"
echo -e " "
fi
