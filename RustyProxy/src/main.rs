use std::io::{self, Error};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt}; // <--- CORREÇÃO APLICADA AQUI
use tokio::net::{TcpListener, TcpStream};
use clap::Parser;
use tracing::{error, info, instrument, warn};
use tokio_tungstenite;
use futures_util::{StreamExt, SinkExt};
use httparse::Status;

// --- Estrutura de Configuração ---
#[derive(Parser, Debug, Clone)]
#[command(author, version, about = "Um proxy TCP/WebSocket dinâmico e robusto.")]
struct Args {
    #[arg(long, short, default_value_t = 80)]
    port: u16,
    #[arg(long, default_value = "@RustyManager")]
    status: String,
    #[arg(long, default_value = "127.0.0.1:22")]
    ssh_addr: String,
    #[arg(long, default_value = "127.0.0.1:1194")]
    default_addr: String,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    // Usando 'tracing' para logging profissional
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
                    if let Err(e) = handle_connection_routing(client_stream, args_clone).await {
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

// --- Roteador Principal ---
#[instrument(skip_all, fields(client_addr = %stream.peer_addr().ok().map_or_else(String::new, |a| a.to_string())))]
async fn handle_connection_routing(stream: TcpStream, args: Arc<Args>) -> Result<(), Box<dyn std::error::Error>> {
    let mut buffer = [0; 4096];
    let bytes_read = stream.peek(&mut buffer).await?;

    if bytes_read == 0 {
        warn!("Cliente conectou e desconectou sem enviar dados.");
        return Ok(());
    }

    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers);
    
    // Tenta analisar como HTTP para detectar WebSocket
    if let Ok(Status::Complete(_)) = req.parse(&buffer[..bytes_read]) {
        if is_websocket_request(&req) {
            info!("Requisição WebSocket detectada.");
            return handle_websocket_proxy(stream, &req).await;
        }
    }
    
    // Se não for WebSocket, trata como TCP genérico
    info!("Conexão não é WebSocket. Tratando como TCP puro.");
    handle_tcp_proxy(stream, &buffer[..bytes_read], args).await?;
    Ok(())
}

// --- Lógica TCP ---
async fn handle_tcp_proxy(mut client_stream: TcpStream, initial_data: &[u8], args: Arc<Args>) -> io::Result<()> {
    // A troca de respostas HTTP para "enganar" o firewall
    client_stream.write_all(format!("HTTP/1.1 101 {}\r\n\r\n", args.status).as_bytes()).await?;
    
    // Consome os dados que já foram lidos pelo peek para não os perdermos
    let mut temp_buf = vec![0; initial_data.len()];
    client_stream.read_exact(&mut temp_buf).await?;

    client_stream.write_all(format!("HTTP/1.1 200 {}\r\n\r\n", args.status).as_bytes()).await?;

    let data_str = String::from_utf8_lossy(initial_data);
    let proxy_addr = if data_str.contains("SSH") {
        info!("Detectado 'SSH'. Redirecionando para {}", &args.ssh_addr);
        &args.ssh_addr
    } else {
        info!("Payload não contém 'SSH'. Redirecionando para {}", &args.default_addr);
        &args.default_addr
    };

    let server_stream_result = TcpStream::connect(proxy_addr).await;
    match server_stream_result {
        Ok(mut server_stream) => {
            info!("Conexão TCP estabelecida com {}. Iniciando proxy.", proxy_addr);
            tokio::io::copy_bidirectional(&mut client_stream, &mut server_stream).await?;
        }
        Err(e) => {
            error!("Falha ao conectar ao destino TCP {}: {}", proxy_addr, e);
            return Err(e);
        }
    }
    Ok(())
}

// --- Lógica WebSocket Dinâmica ---
async fn handle_websocket_proxy<'a>(
    client_stream: TcpStream,
    req: &httparse::Request<'a, '_>,
) -> Result<(), Box<dyn std::error::Error>> {
    let host_header = req.headers.iter().find(|h| h.name.eq_ignore_ascii_case("Host"));
    let Some(host_header) = host_header else {
        warn!("Requisição WebSocket sem cabeçalho Host. Descartando.");
        return Err("Cabeçalho Host ausente na requisição WebSocket".into());
    };
    let host = std::str::from_utf8(host_header.value)?;
    let destination_addr = if host.contains(':') { host.to_string() } else { format!("{}:80", host) };

    info!("Iniciando proxy WebSocket para o destino dinâmico: {}", destination_addr);

    let server_ws_stream_result = tokio_tungstenite::connect_async(&destination_addr).await;
    let (server_ws_stream, _) = match server_ws_stream_result {
        Ok(s) => s,
        Err(e) => {
            error!("Falha ao conectar ao destino WebSocket {}: {}", destination_addr, e);
            return Err(Box::new(e));
        }
    };
    info!("Conexão WebSocket com o servidor de destino estabelecida.");

    let client_ws_stream = tokio_tungstenite::accept_async(client_stream).await?;
    info!("Handshake com o cliente concluído. A conexão foi promovida para WebSocket.");

    let (mut client_write, mut client_read) = client_ws_stream.split();
    let (mut server_write, mut server_read) = server_ws_stream.split();

    let client_to_server = async { while let Some(Ok(msg)) = client_read.next().await { if server_write.send(msg).await.is_err() { break; } } };
    let server_to_client = async { while let Some(Ok(msg)) = server_read.next().await { if client_write.send(msg).await.is_err() { break; } } };
    
    futures_util::future::join(client_to_server, server_to_client).await;
    info!("Conexão WebSocket encerrada para {}", destination_addr);
    Ok(())
}

// --- Função Auxiliar ---
fn is_websocket_request(req: &httparse::Request) -> bool {
    let mut connection_upgrade = false;
    let mut is_upgrade_ws = false;
    for header in req.headers.iter() {
        let key = header.name.to_lowercase();
        let value = String::from_utf8_lossy(header.value).to_lowercase();
        if key == "connection" && value.contains("upgrade") { connection_upgrade = true; }
        if key == "upgrade" && value == "websocket" { is_upgrade_ws = true; }
    }
    connection_upgrade && is_upgrade_ws
}
