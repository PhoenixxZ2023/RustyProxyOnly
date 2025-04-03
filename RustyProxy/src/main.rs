use std::env;
use std::io::Error;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_rustls::{TlsAcceptor, server::TlsStream};
use rustls::{ServerConfig, NoClientAuth};
use tokio_tungstenite::{accept_async, WebSocketStream};
use httparse;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let port = get_port();
    let listener = TcpListener::bind(format!("[::]:{}", port)).await?;
    println!("Iniciando proxy inteligente na porta: {}", port);
    start_proxy(listener).await;
    Ok(())
}

async fn start_proxy(listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((client_stream, addr)) => {
                tokio::spawn(async move {
                    if let Err(e) = handle_client(client_stream).await {
                        println!("Erro ao processar cliente {}: {}", addr, e);
                    }
                });
            }
            Err(e) => {
                println!("Erro ao aceitar conexão: {}", e);
            }
        }
    }
}

async fn handle_client(mut client_stream: TcpStream) -> Result<(), Error> {
    let mut buffer = vec![0; 1024];
    client_stream.read(&mut buffer).await?;

    // Configuração TLS para HTTPS
    let mut tls_config = ServerConfig::new(NoClientAuth::new());
    // Aqui você precisaria carregar certificados reais em um ambiente de produção
    // Por simplicidade, vamos simular sem certificados reais
    let acceptor = TlsAcceptor::from(Arc::new(tls_config));

    // Detectar protocolo
    if buffer.starts_with(b"\x16") {
        // HTTPS (TLS)
        let tls_stream = acceptor.accept(client_stream).await?;
        handle_https(tls_stream).await?;
    } else if buffer.starts_with(b"GET") || buffer.starts_with(b"POST") {
        // HTTP ou WebSocket
        let mut headers = [httparse::EMPTY_HEADER; 16];
        let mut req = httparse::Request::new(&mut headers);
        if req.parse(&buffer).is_ok() {
            if headers.iter().any(|h| h.name == "Upgrade" && h.value == b"websocket") {
                handle_websocket(client_stream).await?;
            } else {
                handle_http(client_stream, &buffer).await?;
            }
        }
    } else if buffer.contains(b"SSH") {
        // SSH (mantido como passthrough simples)
        handle_ssh(client_stream).await?;
    } else {
        println!("Protocolo não reconhecido, ignorando...");
    }

    Ok(())
}

// Handler para HTTP
async fn handle_http(mut client_stream: TcpStream, buffer: &[u8]) -> Result<(), Error> {
    let mut headers = [httparse::EMPTY_HEADER; 16];
    let mut req = httparse::Request::new(&mut headers);
    req.parse(buffer)?;

    let host = headers.iter().find(|h| h.name == "Host").map(|h| String::from_utf8_lossy(h.value).to_string());
    let target_addr = host.unwrap_or_else(|| "0.0.0.0:80".to_string());

    let mut server_stream = TcpStream::connect(&target_addr).await?;
    println!("HTTP: Conectando a {}", target_addr);

    // Encaminhar a requisição inicial
    server_stream.write_all(buffer).await?;

    // Transferência bidirecional
    transfer_streams(client_stream, server_stream).await?;
    Ok(())
}

// Handler para HTTPS (TLS)
async fn handle_https(mut tls_stream: TlsStream<TcpStream>) -> Result<(), Error> {
    let mut buffer = vec![0; 1024];
    tls_stream.read(&mut buffer).await?;

    let mut headers = [httparse::EMPTY_HEADER; 16];
    let mut req = httparse::Request::new(&mut headers);
    req.parse(&buffer)?;

    let host = headers.iter().find(|h| h.name == "Host").map(|h| String::from_utf8_lossy(h.value).to_string());
    let target_addr = host.unwrap_or_else(|| "0.0.0.0:443".to_string());

    let mut server_stream = TcpStream::connect(&target_addr).await?;
    println!("HTTPS: Conectando a {}", target_addr);

    server_stream.write_all(&buffer).await?;
    transfer_streams(tls_stream, server_stream).await?;
    Ok(())
}

// Handler para WebSocket
async fn handle_websocket(client_stream: TcpStream) -> Result<(), Error> {
    let ws_stream = accept_async(client_stream).await?;
    let target_addr = "0.0.0.0:80"; // Em um proxy real, extrairia o destino do handshake
    let mut server_stream = TcpStream::connect(target_addr).await?;
    println!("WebSocket: Conectando a {}", target_addr);

    // Aqui seria necessário implementar o handshake WebSocket com o servidor de destino
    // Por simplicidade, apenas encaminhamos como TCP
    transfer_streams(ws_stream, server_stream).await?;
    Ok(())
}

// Handler para SSH (passthrough)
async fn handle_ssh(client_stream: TcpStream) -> Result<(), Error> {
    let target_addr = "0.0.0.0:22";
    let mut server_stream = TcpStream::connect(target_addr).await?;
    println!("SSH: Conectando a {}", target_addr);
    transfer_streams(client_stream, server_stream).await?;
    Ok(())
}

// Função genérica para transferência bidirecional
async fn transfer_streams<T, U>(client: T, server: U) -> Result<(), Error>
where
    T: AsyncReadExt + AsyncWriteExt + Unpin + Send + 'static,
    U: AsyncReadExt + AsyncWriteExt + Unpin + Send + 'static,
{
    let (client_read, client_write) = tokio::io::split(client);
    let (server_read, server_write) = tokio::io::split(server);

    let client_read = Arc::new(Mutex::new(client_read));
    let client_write = Arc::new(Mutex::new(client_write));
    let server_read = Arc::new(Mutex::new(server_read));
    let server_write = Arc::new(Mutex::new(server_write));

    let client_to_server = transfer_data(client_read, server_write);
    let server_to_client = transfer_data(server_read, client_write);

    tokio::try_join!(client_to_server, server_to_client)?;
    Ok(())
}

async fn transfer_data(
    read_stream: Arc<Mutex<impl AsyncReadExt + Unpin>>,
    write_stream: Arc<Mutex<impl AsyncWriteExt + Unpin>>,
) -> Result<(), Error> {
    let mut buffer = [0; 8192];
    loop {
        let bytes_read = {
            let mut read_guard = read_stream.lock().await;
            read_guard.read(&mut buffer).await?
        };

        if bytes_read == 0 {
            break;
        }

        let mut write_guard = write_stream.lock().await;
        write_guard.write_all(&buffer[..bytes_read]).await?;
    }
    Ok(())
}

fn get_port() -> u16 {
    let args: Vec<String> = env::args().collect();
    let mut port = 80;
    for i in 1..args.len() {
        if args[i] == "--port" && i + 1 < args.len() {
            port = args[i + 1].parse().unwrap_or(80);
        }
    }
    port
}
