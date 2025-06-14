use std::env;
use std::io::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use bytes::BytesMut;
use httparse::{Request, EMPTY_HEADER};
use futures_util::{StreamExt, SinkExt};
use http::Uri;

// Função principal, sem alterações
#[tokio::main]
async fn main() -> Result<(), Error> {
    let port = get_port();
    let listener = TcpListener::bind(format!("[::]:{}", port)).await?;
    println!("Iniciando RustyProxy na porta: {}", port);
    start_http(listener).await;
    Ok(())
}

// Função de loop, sem alterações
async fn start_http(listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((client_stream, addr)) => {
                tokio::spawn(async move {
                    if let Err(e) = handle_client(client_stream).await {
                        if !e.to_string().contains("Connection reset by peer") && !e.to_string().contains("unexpected eof") {
                            println!("Erro ao processar cliente {}: {}", addr, e);
                        }
                    }
                });
            }
            Err(e) => {
                println!("Erro ao aceitar conexão: {}", e);
            }
        }
    }
}

// ================================================================
// A NOVA LÓGICA DE DETECÇÃO ESTÁ AQUI
// ================================================================
async fn handle_client(mut client_stream: TcpStream) -> Result<(), Error> {
    let mut buf = BytesMut::with_capacity(8192);

    let bytes_read = client_stream.read_buf(&mut buf).await?;
    if bytes_read == 0 {
        return Ok(());
    }

    // Critério de decisão:
    // 1. É OpenVPN? (Verifica bytes específicos)
    // 2. Se não, é um upgrade para WebSocket? (Tenta parse HTTP)
    // 3. Se não, é HTTP padrão.

    // Padrão de bytes de um handshake OpenVPN sobre TCP (P_CONTROL_V1)
    // O primeiro byte é o tipo de opcode (0x38), precedido pelo tamanho do pacote.
    // Vamos verificar de forma simples se o opcode está presente nos primeiros bytes.
    let is_openvpn = buf.len() > 2 && buf[2] == 0x38;

    if is_openvpn {
        println!("Detectado tráfego OpenVPN. Encaminhando...");
        let openvpn_addr = "127.0.0.1:1194"; // Porta do seu servidor OpenVPN
        proxy_raw_traffic(client_stream, buf, openvpn_addr).await?;

    } else {
        // Se não for OpenVPN, tentamos tratar como HTTP
        let mut headers = [EMPTY_HEADER; 32];
        let mut req = Request::new(&mut headers);
        
        if req.parse(&buf).is_ok() {
            let mut is_ws = false;
            for h in req.headers.iter() {
                if h.name.eq_ignore_ascii_case("Upgrade") && String::from_utf8_lossy(h.value).eq_ignore_ascii_case("websocket") {
                    if let Some(conn_header) = req.headers.iter().find(|h2| h2.name.eq_ignore_ascii_case("Connection")) {
                        if String::from_utf8_lossy(conn_header.value).eq_ignore_ascii_case("Upgrade") {
                            is_ws = true;
                            break;
                        }
                    }
                }
            }

            if is_ws {
                println!("Detectado Handshake WebSocket. Encaminhando...");
                handle_websocket_proxy(client_stream).await?; // A lib Tungstenite relê o buffer
            } else {
                println!("Detectada requisição HTTP padrão. Encaminhando...");
                let http_addr = "127.0.0.1:80"; // Porta do seu Nginx/Apache
                proxy_raw_traffic(client_stream, buf, http_addr).await?;
            }
        } else {
            // Se não for OpenVPN e não for um HTTP válido, podemos assumir que é SSH
            println!("Protocolo não identificado como OpenVPN ou HTTP. Tentando SSH...");
            let ssh_addr = "127.0.0.1:22"; // Porta do seu servidor SSH
            proxy_raw_traffic(client_stream, buf, ssh_addr).await?;
        }
    }

    Ok(())
}

// Função genérica para encaminhar tráfego bruto (usada por HTTP, OpenVPN e SSH)
async fn proxy_raw_traffic(mut client_stream: TcpStream, initial_buffer: BytesMut, backend_addr: &str) -> Result<(), Error> {
    let mut server_stream = match TcpStream::connect(backend_addr).await {
        Ok(s) => s,
        Err(e) => {
            println!("Erro ao conectar ao backend {}: {}", backend_addr, e);
            return Err(e.into());
        }
    };
    server_stream.write_all(&initial_buffer).await?;
    let (mut client_read, mut client_write) = client_stream.into_split();
    let (mut server_read, mut server_write) = server_stream.into_split();
    tokio::try_join!(
        tokio::io::copy(&mut client_read, &mut server_write),
        tokio::io::copy(&mut server_read, &mut client_write)
    )?;
    Ok(())
}

// Função de proxy WebSocket (simplificada, pois o buffer inicial é lido pela lib)
async fn handle_websocket_proxy(client_tcp_stream: TcpStream) -> Result<(), Error> {
    let ws_target_addr = "ws://127.0.0.1:8080";
    let ws_client_stream = match tokio_tungstenite::accept_async(client_tcp_stream).await {
        Ok(ws) => ws,
        Err(e) => {
            println!("Erro no handshake WebSocket com o cliente: {}", e);
            return Err(Error::new(std::io::ErrorKind::Other, format!("WebSocket handshake failed: {}", e)));
        }
    };
    let uri: Uri = ws_target_addr.parse().expect("URI do WebSocket de destino inválida");
    let (ws_server_stream, _response) = match tokio_tungstenite::connect_async(uri).await {
        Ok(res) => res,
        Err(e) => {
            println!("Erro ao conectar ao servidor WebSocket {}: {}", ws_target_addr, e);
            return Err(Error::new(std::io::ErrorKind::Other, format!("Failed to connect to WebSocket target: {}", e)));
        }
    };
    let (mut ws_client_write, mut ws_client_read) = ws_client_stream.split();
    let (mut ws_server_write, mut ws_server_read) = ws_server_stream.split();
    tokio::spawn(async move { while let Some(Ok(msg)) = ws_client_read.next().await { if ws_server_write.send(msg).await.is_err() { break; } } });
    tokio::spawn(async move { while let Some(Ok(msg)) = ws_server_read.next().await { if ws_client_write.send(msg).await.is_err() { break; } } });
    Ok(())
}

// Funções utilitárias, sem alterações
fn get_arg_value(arg_name: &str, default_value: &str) -> String {
    let args: Vec<String> = env::args().collect();
    for i in 1..args.len() { if args[i] == arg_name { if i + 1 < args.len() { return args[i + 1].clone(); } } }
    default_value.to_string()
}
fn get_port() -> u16 {
    get_arg_value("--port", "80").parse().unwrap_or(80)
}
