use std::env;
use std::io::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::{time::Duration};
use tokio::time::timeout;

// --- Imports de HTTP e bytes ---
use http::Uri;
use bytes::BytesMut;
use httparse::{Request, EMPTY_HEADER};

// --- Imports para futures-util (traits) ---
use futures_util::{StreamExt, SinkExt};


#[tokio::main]
async fn main() -> Result<(), Error> {
    let port = get_port();
    let listener = TcpListener::bind(format!("[::]:{}", port)).await?;
    println!("Iniciando RustyProxy na porta: {}", port);
    start_http(listener).await;
    Ok(())
}

async fn start_http(listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((client_stream, addr)) => {
                tokio::spawn(async move {
                    if let Err(e) = handle_client(client_stream).await {
                        // Evita imprimir erros de "conexão encerrada" que são normais
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

async fn handle_client(mut client_stream: TcpStream) -> Result<(), Error> {
    // Buffer para ler os dados iniciais do cliente
    let mut buf = BytesMut::with_capacity(8192);

    let bytes_read = client_stream.read_buf(&mut buf).await?;
    if bytes_read == 0 {
        return Ok(()); // Conexão fechada pelo cliente sem enviar dados
    }

    // Tenta analisar a requisição como HTTP
    let mut headers = [EMPTY_HEADER; 32];
    let mut req = Request::new(&mut headers);
    let parse_result = req.parse(&buf);

    let is_websocket_upgrade = if parse_result.is_ok() {
        let mut upgrade_found = false;
        let mut connection_upgrade_found = false;

        for h in req.headers.iter() {
            if h.name.eq_ignore_ascii_case("Upgrade") && String::from_utf8_lossy(h.value).eq_ignore_ascii_case("websocket") {
                upgrade_found = true;
            }
            if h.name.eq_ignore_ascii_case("Connection") && String::from_utf8_lossy(h.value).eq_ignore_ascii_case("Upgrade") {
                connection_upgrade_found = true;
            }
        }
        upgrade_found && connection_upgrade_found
    } else {
        false
    };

    if is_websocket_upgrade {
        // CAMINHO 1: A requisição é um upgrade para WebSocket
        println!("Detectado Handshake WebSocket. Encaminhando...");
        handle_websocket_proxy(client_stream, buf).await?;
    } else {
        // CAMINHO 2: A requisição é HTTP padrão (CONNECT, GET, POST, etc.)
        println!("Detectada requisição HTTP padrão. Encaminhando...");
        handle_http_proxy(client_stream, buf).await?;
    }

    Ok(())
}

async fn handle_websocket_proxy(client_tcp_stream: TcpStream, _initial_buffer: BytesMut) -> Result<(), Error> {
    // Endereço do seu servidor WebSocket de backend (ex: um painel v2ray com WS)
    let ws_target_addr = "ws://127.0.0.1:8080";

    // A biblioteca `tokio-tungstenite` lida com o handshake e responde com '101 Switching Protocols'
    let ws_client_stream = match tokio_tungstenite::accept_async(client_tcp_stream).await {
        Ok(ws) => ws,
        Err(e) => {
            println!("Erro no handshake WebSocket com o cliente: {}", e);
            return Err(Error::new(std::io::ErrorKind::Other, format!("WebSocket handshake failed: {}", e)));
        }
    };
    println!("Handshake WebSocket com cliente concluído.");

    let uri: Uri = ws_target_addr.parse().expect("URI do WebSocket de destino inválida");

    let (ws_server_stream, _response) = match tokio_tungstenite::connect_async(uri).await {
        Ok(res) => res,
        Err(e) => {
            println!("Erro ao conectar ao servidor WebSocket de destino {}: {}", ws_target_addr, e);
            return Err(Error::new(std::io::ErrorKind::Other, format!("Failed to connect to WebSocket target: {}", e)));
        }
    };
    println!("Conectado ao servidor WebSocket de destino: {}", ws_target_addr);

    // Divide os streams em leitura e escrita para encaminhar as mensagens
    let (mut ws_client_write, mut ws_client_read) = ws_client_stream.split();
    let (mut ws_server_write, mut ws_server_read) = ws_server_stream.split();

    // Tarefas para encaminhar mensagens em ambas as direções
    let client_to_server_ws = tokio::spawn(async move {
        while let Some(msg_result) = ws_client_read.next().await {
            if let Ok(msg) = msg_result {
                if ws_server_write.send(msg).await.is_err() { break; }
            } else { break; }
        }
    });

    let server_to_client_ws = tokio::spawn(async move {
        while let Some(msg_result) = ws_server_read.next().await {
            if let Ok(msg) = msg_result {
                if ws_client_write.send(msg).await.is_err() { break; }
            } else { break; }
        }
    });

    // Espera que ambas as tarefas terminem
    let _ = tokio::try_join!(client_to_server_ws, server_to_client_ws);
    
    Ok(())
}

async fn handle_http_proxy(mut client_stream: TcpStream, initial_buffer: BytesMut) -> Result<(), Error> {
    // Endereço do seu servidor web de backend (ex: Nginx, Apache)
    // Este servidor receberá todas as requisições CONNECT, GET, POST, etc.
    let http_backend_addr = "127.0.0.1:8080"; 

    let mut server_stream = match TcpStream::connect(http_backend_addr).await {
        Ok(s) => s,
        Err(e) => {
            println!("Erro ao conectar ao backend HTTP {}: {}", http_backend_addr, e);
            let response = "HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n";
            client_stream.write_all(response.as_bytes()).await?;
            return Err(e.into());
        }
    };

    // Envia os dados que já foram lidos do cliente para o servidor de backend
    server_stream.write_all(&initial_buffer).await?;

    // Agora, estabelece o túnel bidirecional usando tokio::io::copy
    let (mut client_read, mut client_write) = client_stream.into_split();
    let (mut server_read, mut server_write) = server_stream.into_split();
    
    let client_to_server = tokio::io::copy(&mut client_read, &mut server_write);
    let server_to_client = tokio::io::copy(&mut server_read, &mut client_write);

    tokio::try_join!(client_to_server, server_to_client)?;

    Ok(())
}

// --- Funções Utilitárias para ler argumentos da linha de comando ---

fn get_arg_value(arg_name: &str, default_value: &str) -> String {
    let args: Vec<String> = env::args().collect();
    for i in 1..args.len() {
        if args[i] == arg_name {
            if i + 1 < args.len() {
                return args[i + 1].clone();
            }
        }
    }
    default_value.to_string()
}

fn get_port() -> u16 {
    get_arg_value("--port", "80").parse().unwrap_or(80)
}
