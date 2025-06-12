use clap::Parser;
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use tokio::io::{self, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{timeout, Duration};
use tracing::{error, info, warn};
use tracing_subscriber;

// 1. Estrutura única para toda a configuração, usando 'clap' para parsing.
#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
struct Config {
    #[arg(long, default_value_t = 80)]
    port: u16,

    #[arg(long, default_value = "@RustyManager")]
    status: String,

    #[arg(long, default_value = "127.0.0.1:22")]
    ssh_target: String,

    #[arg(long, default_value = "127.0.0.1:1194")]
    openvpn_target: String,

    #[arg(long, default_value_t = 2)]
    peek_timeout_secs: u64,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    // Inicializa o logger tracing
    tracing_subscriber::fmt::init();

    // 2. Configuração é lida UMA VEZ no início.
    let config = Arc::new(Config::parse());
    info!("Configurações carregadas: {:?}", config);

    let listener = TcpListener::bind(format!("[::]:{}", config.port)).await?;
    info!("Iniciando serviço na porta: {}", config.port);
    
    start_proxy(listener, config).await;
    Ok(())
}

async fn start_proxy(listener: TcpListener, config: Arc<Config>) {
    loop {
        match listener.accept().await {
            Ok((client_stream, addr)) => {
                info!("Nova conexão de: {}", addr);
                // O Arc<Config> é clonado de forma barata para cada nova task.
                let config_clone = config.clone();
                tokio::spawn(async move {
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
    // 3. O handshake HTTP inicial é feito.
    client_stream
        .write_all(format!("HTTP/1.1 101 {}\r\n\r\n", config.status).as_bytes())
        .await?;
     client_stream
        .write_all(format!("HTTP/1.1 200 {}\r\n\r\n", config.status).as_bytes())
        .await?;

    // 4. CORREÇÃO DA FALHA LÓGICA: Usamos peek() ANTES de qualquer leitura.
    let mut peek_buffer = vec![0; 1024];
    let peek_duration = Duration::from_secs(config.peek_timeout_secs);
    
    let bytes_peeked = match timeout(peek_duration, client_stream.peek(&mut peek_buffer)).await {
        Ok(Ok(n)) => n,
        Ok(Err(e)) => return Err(e), // Erro de I/O no peek
        Err(_) => 0, // Timeout, consideramos como 0 bytes espiados
    };
    
    let data = String::from_utf8_lossy(&peek_buffer[..bytes_peeked]);

    // 5. A decisão de roteamento é feita com base nos dados espiados.
    let target_addr = if data.to_uppercase().contains("SSH") {
        info!("Protocolo detectado: SSH");
        &config.ssh_target
    } else {
        info!("Protocolo detectado: OpenVPN (ou outro)");
        &config.openvpn_target
    };

    info!("Redirecionando para: {}", target_addr);
    let mut server_stream = TcpStream::connect(target_addr).await.map_err(|e| {
        error!("Falha ao conectar ao destino {}: {}", target_addr, e);
        e
    })?;

    // 6. CORREÇÃO DE EFICIÊNCIA: Usamos a função otimizada copy_bidirectional.
    // Isso substitui toda a lógica de Arc<Mutex<...>> e a função transfer_data.
    io::copy_bidirectional(&mut client_stream, &mut server_stream).await?;
    
    Ok(())
}
