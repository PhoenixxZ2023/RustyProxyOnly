use clap::Parser;
use std::io::{Error, ErrorKind};
use std::sync::Arc;
// CORREÇÃO 1: Removido o 'AsyncReadExt' não utilizado
use tokio::io::{self, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::time::{timeout, Duration};
use tracing::{error, info, warn};
use tracing_subscriber;

// Estrutura para gerenciar configurações usando clap para parsing de argumentos
#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
struct Config {
    #[arg(long, default_value_t = 80)]
    port: u16,

    #[arg(long, default_value = "@RustyManager")]
    status: String,

    #[arg(long, default_value = "127.0.0.1:22")]
    ssh_target_addr: String,

    #[arg(long, default_value = "127.0.0.1:1194")]
    openvpn_target_addr: String,

    #[arg(long, default_value = "127.0.0.1:8080")]
    http_target_addr: String,

    #[arg(long, default_value_t = 2)]
    peek_timeout_secs: u64,

    #[arg(long, default_value_t = 30)]
    client_handling_timeout_secs: u64,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    // Inicializa o logger tracing
    tracing_subscriber::fmt::init();

    // Carrega a configuração usando clap
    let config = Arc::new(Config::parse());
    info!("Configurações carregadas: {:?}", config);

    let listener = TcpListener::bind(format!("[::]:{}", config.port)).await?;
    info!("Serviço iniciado na porta: {}", config.port);
    
    start_proxy(listener, config).await;
    Ok(())
}

async fn start_proxy(listener: TcpListener, config: Arc<Config>) {
    let max_connections = Arc::new(Semaphore::new(1000));

    loop {
        // Usa `acquire_owned` para que o permit seja liberado quando a task terminar
        let permit = match max_connections.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => {
                error!("Semaphore foi fechado, encerrando o loop.");
                return;
            }
        };

        match listener.accept().await {
            Ok((client_stream, addr)) => {
                info!("Nova conexão de: {}", addr);
                let config_clone = config.clone();
                tokio::spawn(async move {
                    // O permit é movido para dentro da task, garantindo que ele exista
                    // durante todo o tempo de vida da conexão.
                    let _permit = permit; 
                    if let Err(e) = handle_client(client_stream, config_clone).await {
                        error!("Erro ao processar cliente {}: {}", addr, e);
                    }
                    info!("Conexão com {} encerrada.", addr);
                });
            }
            Err(e) => {
                error!("Erro ao aceitar conexão: {}", e);
            }
        }
    }
}

async fn handle_client(mut client_stream: TcpStream, config: Arc<Config>) -> Result<(), Error> {
    let handle_timeout = Duration::from_secs(config.client_handling_timeout_secs);

    timeout(handle_timeout, async {
        // 1. Inspeciona os dados iniciais sem consumi-los
        let peek_timeout = Duration::from_secs(config.peek_timeout_secs);
        let mut peek_buffer = vec![0; 2048];
        
        let bytes_peeked = timeout(peek_timeout, client_stream.peek(&mut peek_buffer))
            .await
            .map_err(|_| Error::new(ErrorKind::TimedOut, "Timeout ao inspecionar o stream (peek)"))?
            .map_err(|e| {
                warn!("Erro de I/O ao inspecionar o stream: {}", e);
                e
            })?;

        // 2. Realiza o handshake HTTP falso (injector)
        client_stream
            .write_all(format!("HTTP/1.1 101 {}\r\n\r\n", config.status).as_bytes())
            .await?;
        
        client_stream
            .write_all(format!("HTTP/1.1 200 Connection Established\r\n\r\n",).as_bytes())
            .await?;

        // 3. Detecta o protocolo e determina o destino
        let target_addr = detect_protocol(&peek_buffer[..bytes_peeked], &config);
        info!("Redirecionando para: {}", target_addr);

        // 4. Conecta ao servidor de destino
        let mut server_stream = TcpStream::connect(target_addr).await.map_err(|e| {
            error!("Falha ao conectar ao destino {}: {}", target_addr, e);
            e
        })?;

        // 5. Transfere os dados de forma bidirecional
        
        // CORREÇÃO 2: Removidas as chamadas .split() e passamos os streams completos.
        match io::copy_bidirectional(&mut client_stream, &mut server_stream).await {
            Ok((to_server, to_client)) => {
                info!(
                    "Transferência concluída. Bytes para o servidor: {}, Bytes para o cliente: {}",
                    to_server, to_client
                );
                Ok(())
            }
            Err(e) => {
                warn!("Erro durante a transferência de dados: {}", e);
                Err(e)
            }
        }
    }).await.map_err(|_| {
        Error::new(ErrorKind::TimedOut, format!("Timeout de {}s ao manipular cliente", config.client_handling_timeout_secs))
    })?
}


// Função para detectar o protocolo baseado nos dados iniciais
fn detect_protocol<'a>(data: &[u8], config: &'a Config) -> &'a str {
    let request_str = String::from_utf8_lossy(data);

    // Detecção de SSH
    if request_str.trim().starts_with("SSH-2.0") {
        info!("Protocolo detectado: SSH");
        return &config.ssh_target_addr;
    } 
    
    // Detecção de métodos HTTP, incluindo o método "ACL"
    else if request_str.starts_with("GET")
        || request_str.starts_with("POST")
        || request_str.starts_with("CONNECT")
        || request_str.starts_with("PUT")
        || request_str.starts_with("DELETE")
        || request_str.starts_with("HEAD")
        || request_str.starts_with("ACL")
    {
        info!("Protocolo detectado: HTTP");
        return &config.http_target_addr;
    } 
    
    // Se não for nenhum dos anteriores, usa o padrão
    else {
        info!("Protocolo não identificado, redirecionando para OpenVPN (padrão)");
        &config.openvpn_target_addr
    }
}
