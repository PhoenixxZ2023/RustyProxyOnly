use std::io::{self, Error};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt, copy_bidirectional}; // <--- IMPORTAÇÃO ADICIONADA AQUI (indiretamente)
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{timeout, Duration};
use clap::Parser;
use tracing::{error, info, instrument};

#[derive(Parser, Debug, Clone)]
#[command(author, version, about = "Um proxy TCP simples para redirecionar tráfego.")]
struct Args {
    /// A porta em que o proxy irá escutar
    #[arg(long, short, default_value_t = 80)]
    port: u16,

    /// A mensagem de status para a resposta HTTP inicial
    #[arg(long, default_value = "@RustyManager")]
    status: String,

    /// Endereço de destino para tráfego SSH (ex: 127.0.0.1:22)
    #[arg(long, default_value = "127.0.0.1:22")]
    ssh_addr: String,

    /// Endereço de destino para outro tráfego (ex: 127.0.0.1:1194)
    #[arg(long, default_value = "127.0.0.1:1194")]
    default_addr: String,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt::init();
    let args = Arc::new(Args::parse());

    let listener = TcpListener::bind(format!("[::]:{}", args.port)).await?;
    info!("Proxy escutando em: {}", listener.local_addr()?);

    loop {
        match listener.accept().await {
            Ok((client_stream, addr)) => {
                info!("Nova conexão de: {}", addr);
                let args_clone = args.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(client_stream, args_clone).await {
                        error!("Erro ao processar cliente {}: {}", addr, e);
                    }
                });
            }
            Err(e) => {
                error!("Erro ao aceitar conexão: {}", e);
            }
        }
    }
}

#[instrument(skip_all, fields(client_addr = %client_stream.peer_addr().ok().map_or_else(String::new, |a| a.to_string())))]
async fn handle_client(mut client_stream: TcpStream, args: Arc<Args>) -> io::Result<()> {
    // Envia uma resposta inicial para "enganar" firewalls
    client_stream
        .write_all(b"HTTP/1.1 101 Switching Protocols\r\n\r\n")
        .await?;

    // Lê o primeiro pacote apenas para prosseguir
    let mut buffer = [0; 1024];
    client_stream.read(&mut buffer).await?;
    
    client_stream
        .write_all(format!("HTTP/1.1 200 {}\r\n\r\n", args.status).as_bytes())
        .await?;

    // Lógica de detecção de protocolo
    let mut peek_buffer = [0; 1024];
    let proxy_addr = match timeout(Duration::from_secs(1), client_stream.peek(&mut peek_buffer)).await {
        // Sucesso ao espiar os dados
        Ok(Ok(bytes_peeked)) => {
            let data_str = String::from_utf8_lossy(&peek_buffer[..bytes_peeked]);
            if data_str.contains("SSH") || data_str.is_empty() {
                info!("Detectado 'SSH' ou payload vazio. Redirecionando para {}", &args.ssh_addr);
                &args.ssh_addr
            } else {
                info!("Payload não contém 'SSH'. Redirecionando para {}", &args.default_addr);
                &args.default_addr
            }
        }
        // Timeout ou erro ao espiar os dados
        _ => {
            info!("Timeout ou erro ao espiar dados. Usando padrão SSH: {}", &args.ssh_addr);
            &args.ssh_addr
        }
    };
    
    info!("Conectando ao servidor de destino: {}", proxy_addr);
    let mut server_stream = TcpStream::connect(proxy_addr).await?;
    info!("Conexão estabelecida. Iniciando proxy bidirecional.");

    // --- CHAMADA DA FUNÇÃO CORRIGIDA ---
    let (bytes_sent, bytes_received) = copy_bidirectional(&mut client_stream, &mut server_stream).await?;

    info!("Conexão fechada. Bytes enviados: {}, Bytes recebidos: {}", bytes_sent, bytes_received);

    Ok(())
}
