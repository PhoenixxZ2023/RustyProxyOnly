use std::env;
use std::io::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::{time::Duration};
use tokio::time::timeout;

// --- Alterado para tokio-tungstenite ---
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};
use http::Uri; // Ainda útil para parsear URLs de destino
use bytes::BytesMut;
use httparse::{Request, EMPTY_HEADER};

// Importações para split do stream (de futures_util, puxada por tokio-tungstenite)
use futures_util::{StreamExt, SinkExt}; // Estes traits são necessários para .next() e .send()


#[tokio::main]
async fn main() -> Result<(), Error> {
    let port = get_port();
    let listener = TcpListener::bind(format!("[::]:{}", port)).await?;
    println!("Iniciando serviço na porta: {}", port);
    start_http(listener).await;
    Ok(())
}

async fn start_http(listener: TcpListener) {
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
    let mut buf = BytesMut::with_capacity(4096);

    let bytes_read = client_stream.read_buf(&mut buf).await?;
    if bytes_read == 0 {
        return Ok(());
    }

    let mut headers = [EMPTY_HEADER; 16];
    let mut req = Request::new(&mut headers);
    
    let parse_status = req.parse(&buf)
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, format!("Erro de parsing HTTP: {}", e)))?;

    let is_websocket_upgrade = if let httparse::Status::Complete(offset) = parse_status {
        // Ignorar o offset por enquanto, ele não está sendo usado
        let _ = offset; 
        let mut upgrade_found = false;
        let mut connection_upgrade_found = false;

        for h in req.headers.iter() {
            if h.name.eq_ignore_ascii_case("Upgrade") && String::from_utf8_lossy(h.value).eq_ignore_ascii_case("websocket") {
                upgrade_found = true;
            }
            // "Connection: upgrade" ou "Connection: Upgrade"
            if h.name.eq_ignore_ascii_case("Connection") && String::from_utf8_lossy(h.value).eq_ignore_ascii_case("Upgrade") {
                connection_upgrade_found = true;
            }
        }
        upgrade_found && connection_upgrade_found
    } else {
        false
    };

    if is_websocket_upgrade {
        println!("Detectado Handshake WebSocket!");
        // tokio-tungstenite::accept_async_with_config lida com o buffer inicial
        handle_websocket_proxy(client_stream, buf).await?; // Passa BytesMut, não Bytes
    } else {
        let status = get_status();
        client_stream
            .write_all(format!("HTTP/1.1 101 {}\r\n\r\n", status).as_bytes())
            .await?;

        // Consumir o buffer restante para evitar problemas no próximo read/peek
        let _ = client_stream.read(&mut buf).await?; // Lê qualquer coisa que sobrou ou próximo pacote
        
        client_stream
            .write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes())
            .await?;

        let peek_result = timeout(Duration::from_secs(1), peek_stream(&mut client_stream)).await;
        let data_to_check = match peek_result {
            Ok(Ok(data)) => data,
            Ok(Err(e)) => {
                println!("Erro ao espiar o stream: {}", e);
                String::new()
            },
            Err(_) => {
                println!("Tempo limite excedido ao espiar o stream.");
                String::new()
            }
        };

        let addr_proxy = if data_to_check.contains("SSH") || data_to_check.is_empty() {
            "0.0.0.0:22"
        } else {
            "0.0.0.0:1194"
        };

        let server_connect = TcpStream::connect(addr_proxy).await;
        let server_stream = match server_connect {
            Ok(s) => s,
            Err(e) => {
                println!("Erro ao iniciar conexão para o proxy {}: {}", addr_proxy, e);
                return Ok(());
            }
        };

        let (client_read, client_write) = client_stream.into_split();
        let (server_read, server_write) = server_stream.into_split();

        let client_to_server = transfer_data(client_read, server_write);
        let server_to_client = transfer_data(server_read, client_write);

        tokio::try_join!(client_to_server, server_to_client)?;
    }

    Ok(())
}

// --- Nova Função: handle_websocket_proxy (usando tokio-tungstenite) ---
async fn handle_websocket_proxy(
    client_tcp_stream: TcpStream,
    initial_data_buf: BytesMut, // Agora recebe BytesMut para accept_async_with_config
) -> Result<(), Error> {
    println!("Iniciando proxy WebSocket...");

    // Configura um configurador de handshake com o buffer já lido
    let mut config = tokio_tungstenite::tungstenite::handshake::server::AcceptorBuilder::default();
    config.extra_headers(Some(initial_data_buf.to_vec().into())); // Isso pode precisar de ajuste, se o extra_headers for para a resposta, não a requisição.
                                                                 // Melhor: apenas aceitar diretamente o stream e o buffer

    // accept_async_with_config é o que nos permite passar o buffer inicial.
    // Ele espera um TcpStream e um buffer ReadBuf.
    let ws_client_stream = match tokio_tungstenite::accept_async_with_config(
        client_tcp_stream,
        Some(tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default().max_send_queue(None).max_message_size(None).max_frame_size(None)),
        Some(initial_data_buf.freeze()), // Passa o Bytes como ReadBuf
    ).await {
        Ok(ws) => ws,
        Err(e) => {
            println!("Erro no handshake WebSocket com o cliente: {}", e);
            return Err(Error::new(std::io::ErrorKind::Other, format!("WebSocket handshake failed: {}", e)));
        }
    };
    println!("Handshake WebSocket com cliente concluído.");

    // Conectar-se ao servidor WebSocket de destino.
    let ws_target_addr = "ws://127.0.0.1:8081"; // Ajuste para o seu servidor WebSocket real
    let uri: Uri = ws_target_addr.parse().expect("URI inválida");

    let (ws_server_stream, _response) = match tokio_tungstenite::connect_async(uri).await {
        Ok(res) => res,
        Err(e) => {
            println!("Erro ao conectar ao servidor WebSocket {}: {}", ws_target_addr, e);
            return Err(Error::new(std::io::ErrorKind::Other, format!("Failed to connect to WebSocket target: {}", e)));
        }
    };
    println!("Conectado ao servidor WebSocket de destino: {}", ws_target_addr);

    // Encaminhar frames WebSocket entre os dois streams.
    // O split() é um método direto do WebSocketStream da tokio-tungstenite
    let (mut ws_client_write, mut ws_client_read) = ws_client_stream.split();
    let (mut ws_server_write, mut ws_server_read) = ws_server_stream.split();

    // Tarefa: Cliente -> Servidor (WebSocket)
    let client_to_server_ws = tokio::spawn(async move {
        while let Some(msg_result) = ws_client_read.next().await {
            match msg_result {
                Ok(msg) => {
                    if let Err(e) = ws_server_write.send(msg).await {
                        println!("Erro ao enviar msg do cliente para o servidor WS: {}", e);
                        break;
                    }
                },
                Err(e) => {
                    println!("Erro ao receber msg do cliente WS: {}", e);
                    break;
                }
            }
        }
        println!("Conexão WS cliente -> servidor encerrada.");
        Ok::<(), Error>(())
    });

    // Tarefa: Servidor -> Cliente (WebSocket)
    let server_to_client_ws = tokio::spawn(async move {
        while let Some(msg_result) = ws_server_read.next().await {
            match msg_result {
                Ok(msg) => {
                    if let Err(e) = ws_client_write.send(msg).await {
                        println!("Erro ao enviar msg do servidor para o cliente WS: {}", e);
                        break;
                    }
                },
                Err(e) => {
                    println!("Erro ao receber msg do servidor WS: {}", e);
                    break;
                }
            }
        }
        println!("Conexão WS servidor -> cliente encerrada.");
        Ok::<(), Error>(())
    });

    tokio::try_join!(client_to_server_ws, server_to_client_ws)?;

    Ok(())
}

// Funções existentes (sem mudanças)
async fn transfer_data(
    mut read_stream: tokio::net::tcp::OwnedReadHalf,
    mut write_stream: tokio::net::tcp::OwnedWriteHalf,
) -> Result<(), Error> {
    let mut buffer = [0; 8192];
    loop {
        let bytes_read = read_stream.read(&mut buffer).await?;

        if bytes_read == 0 {
            break;
        }

        write_stream.write_all(&buffer[..bytes_read]).await?;
    }
    Ok(())
}

async fn peek_stream(stream: &TcpStream) -> Result<String, Error> {
    let mut peek_buffer = vec![0; 8192];
    let bytes_peeked = stream.peek(&mut peek_buffer).await?;
    let data = &peek_buffer[..bytes_peeked];
    let data_str = String::from_utf8_lossy(data);
    Ok(data_str.to_string())
}

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

fn get_status() -> String {
    get_arg_value("--status", "@RustyManager")
}
