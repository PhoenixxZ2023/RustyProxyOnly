use std::env;
use std::io::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{timeout, Duration};

// Dependências necessárias para o novo suporte a WebSocket e HTTP parsing
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
    // Passo 1: Ler os dados iniciais para detectar o protocolo. É a única mudança necessária no fluxo.
    let mut buf = BytesMut::with_capacity(4096);
    client_stream.read_buf(&mut buf).await?;

    // Passo 2: Verificar se é uma requisição de upgrade para WebSocket
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

    // Passo 3: Decidir qual lógica usar
    if is_websocket {
        // ---- SE FOR WEBSOCKET, USA A NOVA LÓGICA ----
        println!("Detectado Handshake WebSocket. Iniciando proxy WebSocket...");
        // A função de proxy WS precisa do stream original, não do buffer, pois a lib faz a leitura.
        let (client_reader, client_writer) = tokio::io::split(client_stream);
        let combined_stream = tokio::io::join(client_reader, client_writer);
        
        handle_websocket_proxy(combined_stream.0).await?;

    } else {
        // ---- SE NÃO FOR WEBSOCKET, EXECUTA O SEU CÓDIGO ORIGINAL ----
        println!("Protocolo não é WebSocket. Usando lógica original para SSH/OpenVPN...");

        // Início da sua Lógica Original (com pequenas adaptações)
        let status = get_status();
        
        // Escreve os dados que já lemos de volta no stream antes de continuarmos
        let (mut client_reader, mut client_writer) = client_stream.into_split();
        client_writer.write_all(&buf).await?;

        // Respostas Duplas
        client_writer.write_all(format!("HTTP/1.1 101 {}\r\n\r\n", status).as_bytes()).await?;
        client_writer.write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes()).await?;

        // Recombina o stream para o peek
        let mut combined_stream = tokio::io::join(client_reader, client_writer).0;

        // Peek e decisão de proxy
        let mut addr_proxy = "0.0.0.0:22";
        if let Ok(Ok(data)) = timeout(Duration::from_secs(1), peek_stream(&combined_stream)).await {
            if !data.contains("SSH") && !data.is_empty() {
                addr_proxy = "0.0.0.0:1194";
            }
        }

        println!("Redirecionando para: {}", addr_proxy);
        let server_stream = TcpStream::connect(addr_proxy).await?;

        // Encaminhamento do tráfego
        let (mut client_read_final, mut client_write_final) = combined_stream.into_split();
        let (mut server_read, mut server_write) = server_stream.into_split();

        tokio::try_join!(
            tokio::io::copy(&mut client_read_final, &mut server_write),
            tokio::io::copy(&mut server_read, &mut client_write_final)
        )?;
        // Fim da sua Lógica Original
    }

    Ok(())
}

async fn handle_websocket_proxy(client_stream: TcpStream) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ws_client_stream = tokio_tungstenite::accept_async(client_stream).await?;
    println!("Cliente WebSocket conectado.");

    let backend_ws_addr = "ws://127.0.0.1:8080";
    let uri: Uri = backend_ws_addr.parse()?;
    let (ws_server_stream, _) = tokio_tungstenite::connect_async(uri).await?;
    println!("Conectado ao backend WebSocket.");

    let (mut client_write, mut client_read) = ws_client_stream.split();
    let (mut server_write, mut server_read) = ws_server_stream.split();

    let c2s = tokio::spawn(async move {
        while let Some(Ok(msg)) = client_read.next().await {
            if server_write.send(msg).await.is_err() { break; }
        }
    });

    let s2c = tokio::spawn(async move {
        while let Some(Ok(msg)) = server_read.next().await {
            if client_write.send(msg).await.is_err() { break; }
        }
    });

    tokio::select! { _ = c2s => {}, _ = s2c => {} }
    println!("Conexão WebSocket encerrada.");
    Ok(())
}

async fn peek_stream(stream: &TcpStream) -> Result<String, Error> {
    let mut peek_buffer = vec![0; 2048];
    let bytes_peeked = stream.peek(&mut peek_buffer).await?;
    Ok(String::from_utf8_lossy(&peek_buffer[..bytes_peeked]).to_string())
}

// Funções utilitárias do código original
fn get_port() -> u16 {
    env::args().nth(1).unwrap_or_else(|| "80".to_string()).parse().unwrap_or(80)
}

fn get_status() -> String {
    env::args().nth(2).unwrap_or_else(|| "@RustyManager".to_string())
}
