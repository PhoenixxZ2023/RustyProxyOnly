use std::env;
use std::io::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// Dependências necessárias
use bytes::BytesMut;
use futures_util::{SinkExt, StreamExt};
use http::Uri;
use httparse::{Request, EMPTY_HEADER};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let port = get_port();
    let listener = TcpListener::bind(format!("[::]:{}", port)).await?;
    println!("Iniciando RustyProxy v2.1 (peek) na porta: {}", port);
    start_proxy(listener).await;
    Ok(())
}

async fn start_proxy(listener: TcpListener) {
    loop {
        if let Ok((client_stream, addr)) = listener.accept().await {
            tokio::spawn(async move {
                if let Err(e) = handle_client(client_stream).await {
                    if !e.to_string().contains("Handshake failed") {
                        println!("Erro ao processar cliente {}: {}", addr, e);
                    }
                }
            });
        }
    }
}

// Lógica de detecção foi atualizada para usar peek()
async fn handle_client(mut client_stream: TcpStream) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut buf = BytesMut::with_capacity(8192);
    // Usamos PEEK em vez de READ. Os dados não são consumidos do stream.
    let bytes_peeked = client_stream.peek(&mut buf).await?;
    if bytes_peeked == 0 {
        return Ok(());
    }

    let peeked_data = &buf[..bytes_peeked];

    let is_openvpn = peeked_data.len() > 2 && peeked_data[2] == 0x38;

    if is_openvpn {
        println!("Detectado tráfego OpenVPN. Encaminhando...");
        let openvpn_addr = "127.0.0.1:1194";
        proxy_raw_traffic(client_stream, openvpn_addr).await?;
    } else {
        let mut headers = [EMPTY_HEADER; 32];
        let mut req = Request::new(&mut headers);
        
        if req.parse(peeked_data).is_ok() {
            let is_ws = req.headers.iter().any(|h| h.name.eq_ignore_ascii_case("Upgrade") && String::from_utf8_lossy(h.value).eq_ignore_ascii_case("websocket"));

            if is_ws {
                println!("Detectado Handshake WebSocket. Encaminhando...");
                // Passamos o stream inteiro, a biblioteca lê por si mesma.
                handle_websocket_proxy(client_stream).await?;
            } else {
                println!("Detectada requisição HTTP padrão. Encaminhando...");
                let http_addr = "127.0.0.1:8080";
                proxy_raw_traffic(client_stream, http_addr).await?;
            }
        } else {
            println!("Protocolo não identificado. Encaminhando para SSH...");
            let ssh_addr = "127.0.0.1:22";
            proxy_raw_traffic(client_stream, ssh_addr).await?;
        }
    }
    Ok(())
}

// proxy_raw_traffic foi simplificado. Ele não precisa mais receber o buffer inicial.
async fn proxy_raw_traffic(mut client_stream: TcpStream, backend_addr: &str) -> Result<(), Error> {
    let mut server_stream = match TcpStream::connect(backend_addr).await {
        Ok(s) => s,
        Err(e) => {
            println!("Erro ao conectar ao backend {}: {}", backend_addr, e);
            return Err(e.into());
        }
    };
    
    // O tokio::io::copy vai ler os dados do cliente (que não foram consumidos pelo peek) e enviar.
    tokio::io::copy_bidirectional(&mut client_stream, &mut server_stream).await?;
    
    Ok(())
}

// handle_websocket_proxy foi simplificado. Ele não precisa mais do buffer.
async fn handle_websocket_proxy(client_stream: TcpStream) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // A função accept_async lê o handshake do stream por si mesma.
    let ws_client_stream = tokio_tungstenite::accept_async(client_stream).await?;
    println!("Cliente WebSocket conectado.");

    let backend_ws_addr = "ws://127.0.0.1:8081";
    let uri: Uri = backend_ws_addr.parse()?;
    let (ws_server_stream, _) = tokio_tungstenite::connect_async(uri).await?;
    println!("Conectado ao backend WebSocket.");

    let (mut client_write, mut client_read) = ws_client_stream.split();
    let (mut server_write, mut server_read) = ws_server_stream.split();

    let c2s = tokio::spawn(async move { while let Some(Ok(msg)) = client_read.next().await { if server_write.send(msg).await.is_err() { break; } } });
    let s2c = tokio::spawn(async move { while let Some(Ok(msg)) = server_read.next().await { if client_write.send(msg).await.is_err() { break; } } });
    
    tokio::select! { _ = c2s => {}, _ = s2c => {} }
    println!("Conexão WebSocket encerrada.");
    Ok(())
}

// Funções utilitárias
fn get_port() -> u16 { env::args().nth(1).unwrap_or_else(|| "80".to_string()).parse().unwrap_or(80) }
