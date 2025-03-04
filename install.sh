#!/bin/bash
# rustyproxy Installer (Versão Corrigida)

TOTAL_STEPS=10
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

validate_dependencies() {
    local deps=("curl" "git" "build-essential" "pkg-config" "libssl-dev")
    for dep in "${deps[@]}"; do
        if ! dpkg -l | grep -q "^ii  $dep"; then
            apt-get install -y "$dep" > /dev/null 2>&1 || error_exit "Falha ao instalar $dep"
        fi
    done
}

if [ "$EUID" -ne 0 ]; then
    error_exit "Execute o script como root: sudo ./installer.sh"
else
    clear
    
    # Passo 1: Atualizar repositórios
    show_progress "Atualizando repositórios..."
    export DEBIAN_FRONTEND=noninteractive
    apt update -y > /dev/null 2>&1 || error_exit "Falha ao atualizar os repositórios"
    increment_step

    # Passo 2: Verificar sistema
    show_progress "Verificando o sistema..."
    validate_dependencies
    increment_step

    # Passo 3: Validar versão do sistema
    show_progress "Validando versão do sistema..."
    source /etc/os-release
    case $ID in
        ubuntu|debian)
            [[ "$VERSION_ID" =~ "18.04"|"20.04"|"22.04"|"11"|"12" ]] || error_exit "Versão não suportada"
            ;;
        *)
            error_exit "Sistema não suportado"
            ;;
    esac
    increment_step

    # Passo 4: Atualizar sistema
    show_progress "Atualizando o sistema..."
    apt-get upgrade -y > /dev/null 2>&1 || error_exit "Falha na atualização"
    increment_step

    # Passo 5: Criar diretório
    show_progress "Criando diretório..."
    mkdir -p /opt/rustyproxy && chmod 755 /opt/rustyproxy
    increment_step

    # Passo 6: Instalar Rust
    show_progress "Instalando Rust..."
    export CARGO_HOME=/opt/rustyproxy/.cargo
    export RUSTUP_HOME=/opt/rustyproxy/.rustup
    if ! command -v rustc &> /dev/null; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable > /dev/null 2>&1
        source $CARGO_HOME/env
    fi
    increment_step

    # Passo 7: Compilar projeto
    show_progress "Compilando RustyProxy..."
    temp_dir=$(mktemp -d)
    git clone --depth 1 --branch main https://github.com/PhoenixxZ2023/RustyProxyOnly.git "$temp_dir" > /dev/null 2>&1
    
    # Configuração de build otimizada
    export RUSTFLAGS="-C target-cpu=native"
    export CARGO_INCREMENTAL=1
    
    cd "$temp_dir/RustyProxy" || error_exit "Diretório do projeto não encontrado"
    cargo build --release > build.log 2>&1 || {
        echo "=== LOG DE ERRO ==="
        tail -n 20 build.log
        error_exit "Falha na compilação"
    }
    increment_step

    # Passo 8: Instalar binários
    show_progress "Instalando binários..."
    install -m 755 "$temp_dir/RustyProxy/target/release/RustyProxy" /opt/rustyproxy/proxy
    install -m 755 "$temp_dir/menu.sh" /opt/rustyproxy/menu
    ln -sf /opt/rustyproxy/menu /usr/local/bin/rustyproxy
    increment_step

    # Passo 9: Limpeza
    show_progress "Finalizando instalação..."
    rm -rf "$temp_dir"
    ldconfig
    increment_step

    echo -e "\nInstalação concluída! Use:"
    echo -e "Comando: rustyproxy"
    echo -e "Diretório: /opt/rustyproxy"
fi
