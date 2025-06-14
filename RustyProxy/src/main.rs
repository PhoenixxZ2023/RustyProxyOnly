use std::env;
use std::io::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{timeout, Duration};

// Dependências necessárias
use bytes::BytesMut;
use futures_util::{SinkExt, StreamExt};
use http::Uri;
use httparse::{Request, EMPTY_HEADER};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let port = get_port();
    let listener = TcpListener::bind(format!("[::]:{}", port)).await?;
    println!("Iniciando proxy (SSH/OpenVPN + WebSocket) na porta: {}", port);
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

async fn handle_client(mut client_stream: TcpStream) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut buf = BytesMut::with_capacity(4096);
    client_stream.read_buf(&mut buf).await?;

    let mut headers = [EMPTY_HEADER; 16];
    let mut req = Request::new(&mut headers);
    let mut is_websocket = false;
    if req.parse(&buf).is_ok() {
        if let (Some(upgrade), Some(connection)) = (
            req.headers.iter().find(|h| h.name.eq_ignore_ascii_case("Upgrade")),
            req.headers.iter().find(|h| h.name.eq_ignore_ascii_case("Connection")),
        ) {
            if String::from_utf8_lossy(upgrade.value).eq_ignore_ascii_case("websocket")
                && String::from_utf8_lossy(connection.value).contains("Upgrade")
            {
                is_websocket = true;
            }
        }
    }

    if is_websocket {
        println!("Detectado Handshake WebSocket. Iniciando proxy WebSocket...");
        // CORREÇÃO: Passamos o stream diretamente. A biblioteca de websocket
        // sabe como usar o buffer que já foi lido.
        handle_websocket_proxy(client_stream, buf).await?;

    } else {
        println!("Protocolo não é WebSocket. Usando lógica original para SSH/OpenVPN...");
        
        let status = get_status();
        
        // CORREÇÃO: Escrevemos no stream original, sem fazer split antes da hora.
        client_stream.write_all(format!("HTTP/1.1 101 {}\r\n\r\n", status).as_bytes()).await?;
        client_stream.write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes()).await?;

        // CORREÇÃO: Usamos o stream original para o peek.
        let data_peeked = match timeout(Duration::from_secs(1), peek_stream(&client_stream, &buf)).await {
            Ok(Ok(data)) => data,
            _ => String::new(),
        };
        
        let addr_proxy = if !data_peeked.contains("SSH") && !data_peeked.is_empty() {
            "0.0.0.0:1194"
        } else {
            "0.0.0.0:22"
        };

        println!("Redirecionando para: {}", addr_proxy);
        let server_stream = TcpStream::connect(addr_proxy).await?;

        // AGORA SIM: No final, fazemos o split para o encaminhamento
        let (mut client_read, mut client_write) = client_stream.into_split();
        let (mut server_read, mut server_write) = server_stream.into_split();
        
        // Escreve o buffer inicial que não foi enviado para o backend
        server_write.write_all(&buf).await?;

        tokio::try_join!(
            tokio::io::copy(&mut client_read, &mut server_write),
            tokio::io::copy(&mut server_read, &mut client_write)
        )?;
    }

    Ok(())
}

async fn handle_websocket_proxy(client_stream: TcpStream, initial_buffer: BytesMut) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // A função `accept_with_request` permite usar um buffer com dados já lidos
    let request = tokio_tungstenite::tungstenite::handshake::server::create_request(&initial_buffer)?;
    let ws_client_stream = tokio_tungstenite::accept_async_with_config(client_stream, Some(request.into()), None).await?;

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

// A função peek agora considera os dados que já podem estar no buffer
async fn peek_stream(stream: &TcpStream, initial_buf: &BytesMut) -> Result<String, Error> {
    if !initial_buf.is_empty() {
        return Ok(String::from_utf8_lossy(initial_buf).to_string());
    }
    let mut peek_buffer = vec![0; 1024];
    let bytes_peeked = stream.peek(&mut peek_buffer).await?;
    Ok(String::from_utf8_lossy(&peek_buffer[..bytes_peeked]).to_string())
}

fn get_port() -> u16 { env::args().nth(1).unwrap_or_else(|| "80".to_string()).parse().unwrap_or(80) }
fn get_status() -> String { env::args().nth(2).unwrap_or_else(|| "@RustyManager".to_string()) }
