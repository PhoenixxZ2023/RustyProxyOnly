use std::env;
use std::io::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt}; // Correção: AsyncWriteExt importado corretamente
use tokio::net::{TcpListener, TcpStream};
use tokio::{time::Duration};
use tokio::time::timeout;

// --- Imports para suporte a WebSocket ---
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig; // Para configurar o WebSocket
use http::Uri; // Usado para parsear URLs de destino WebSocket
use bytes::BytesMut; // Buffer para leitura inicial de requisições HTTP
use httparse::{Request, EMPTY_HEADER}; // Para analisar cabeçalhos HTTP
use futures_util::{StreamExt, SinkExt}; // Para os métodos .next() e .send() em streams assíncronos


#[tokio::main]
async fn main() -> Result<(), Error> {
    // Iniciando o proxy
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
    // Buffer para tentar ler a requisição HTTP inicial do cliente
    let mut buf = BytesMut::with_capacity(4096); 
    
    let bytes_read = client_stream.read_buf(&mut buf).await?;
    if bytes_read == 0 {
        return Ok(()); // Cliente desconectou imediatamente
    }

    // Tentar analisar a requisição HTTP para detectar WebSocket
    let mut headers = [EMPTY_HEADER; 16];
    let mut req = Request::new(&mut headers);
    
    let parse_status = req.parse(&buf)
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, format!("Erro de parsing HTTP: {}", e)))?;

    let is_websocket_upgrade = if let httparse::Status::Complete(_offset) = parse_status { // _offset para remover warning de variável não usada
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
        false // Se a requisição não estiver completa ou for um erro de parsing, não é um upgrade WebSocket válido
    };

    if is_websocket_upgrade {
        println!("Detectado Handshake WebSocket!");
        // O `tokio-tungstenite::accept_async_with_config` usará o que já está no buffer do `client_stream`
        handle_websocket_proxy(client_stream).await?;
    } else {
        // Lógica existente para SSH/OpenVPN (se não for WebSocket)
        let status = get_status();
        client_stream
            .write_all(format!("HTTP/1.1 101 {}\r\n\r\n", status).as_bytes())
            .await?;

        // Consumir o buffer restante se houver, para não afetar o peek_stream
        let mut temp_buf = vec![0; 1024]; 
        let _ = client_stream.read(&mut temp_buf).await?;
        
        client_stream
            .write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes())
            .await?;

        let peek_result = timeout(Duration::from_secs(1), peek_stream(&mut client_stream)).await;

        let data_to_check = match peek_result {
            Ok(Ok(data)) => data, // Sucesso em peek_stream dentro do timeout
            Ok(Err(e)) => {
                println!("Erro ao espiar o stream: {}", e);
                String::new() // Continua, mas com uma string vazia
            },
            Err(_) => {
                // Timeout ocorreu
                println!("Tempo limite excedido ao espiar o stream.");
                String::new() // Continua, mas com uma string vazia
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
                return Ok(()); // Retorna Ok(()) para a tarefa não falhar, mas o erro é logado.
            }
        };

        let (client_read, client_write) = client_stream.into_split();
        let (server_read, server_write) = server_stream.into_split();

        let client_to_server = transfer_data(client_read, server_write);
        let server_to_client = transfer_data(server_read, client_write);

        // try_join! para comunicação bidirecional
        tokio::try_join!(client_to_server, server_to_client)?;
    }

    Ok(())
}

/// Lida com a conexão WebSocket, atuando como proxy entre o cliente e um servidor WebSocket de destino.
async fn handle_websocket_proxy(
    client_tcp_stream: TcpStream,
) -> Result<(), Error> {
    println!("Iniciando proxy WebSocket...");

    // Aceita a conexão WebSocket do cliente. `tokio-tungstenite` usa o buffer já lido.
    let ws_client_stream = match tokio_tungstenite::accept_async_with_config(
        client_tcp_stream,
        Some(WebSocketConfig {
            max_message_size: None, // Configurações opcionais da WebSocket, use None para valores padrão
            max_frame_size: None,
            ..Default::default()    // Importante: `..Default::default()` DEVE ser o último campo
        }),
    ).await {
        Ok(ws) => ws,
        Err(e) => {
            println!("Erro no handshake WebSocket com o cliente: {}", e);
            return Err(Error::new(std::io::ErrorKind::Other, format!("WebSocket handshake failed: {}", e)));
        }
    };
    println!("Handshake WebSocket com cliente concluído.");

    // Conecta ao servidor WebSocket de destino
    let ws_target_addr = "ws://127.0.0.1:8081"; // Ajuste esta URI para o seu servidor WebSocket real
    let uri: Uri = ws_target_addr.parse().expect("URI inválida para o servidor WebSocket de destino");

    let (ws_server_stream, _response) = match tokio_tungstenite::connect_async(uri).await {
        Ok(res) => res,
        Err(e) => {
            println!("Erro ao conectar ao servidor WebSocket {}: {}", ws_target_addr, e);
            return Err(Error::new(std::io::ErrorKind::Other, format!("Falha ao conectar ao destino WS: {}", e)));
        }
    };
    println!("Conectado ao servidor WebSocket de destino: {}", ws_target_addr);

    // Divide os streams WebSocket em partes de leitura e escrita
    let (mut ws_client_write, mut ws_client_read) = ws_client_stream.split();
    let (mut ws_server_write, mut ws_server_read) = ws_server_stream.split();

    // Tarefa para encaminhar mensagens do cliente para o servidor WebSocket
    let client_to_server_task = tokio::spawn(async move {
        let result: Result<(), Error> = async {
            while let Some(msg_result) = ws_client_read.next().await {
                // Converte o erro de tungstenite::Error para std::io::Error
                let msg = msg_result.map_err(|e| Error::new(std::io::ErrorKind::Other, format!("Erro de leitura WS do cliente: {}", e)))?;
                if let Err(e) = ws_server_write.send(msg).await {
                    println!("Erro ao enviar msg do cliente para o servidor WS: {}", e);
                    return Err(Error::new(std::io::ErrorKind::BrokenPipe, format!("Erro de escrita WS para servidor: {}", e)));
                }
            }
            Ok(())
        }.await;
        println!("Conexão WS cliente -> servidor encerrada.");
        result
    });

    // Tarefa para encaminhar mensagens do servidor WebSocket para o cliente
    let server_to_client_task = tokio::spawn(async move {
        let result: Result<(), Error> = async {
            while let Some(msg_result) = ws_server_read.next().await {
                // Converte o erro de tungstenite::Error para std::io::Error
                let msg = msg_result.map_err(|e| Error::new(std::io::ErrorKind::Other, format!("Erro de leitura WS do servidor: {}", e)))?;
                if let Err(e) = ws_client_write.send(msg).await {
                    println!("Erro ao enviar msg do servidor para o cliente WS: {}", e);
                    return Err(Error::new(std::io::ErrorKind::BrokenPipe, format!("Erro de escrita WS para cliente: {}", e)));
                }
            }
            Ok(())
        }.await;
        println!("Conexão WS servidor -> cliente encerrada.");
        result
    });

    // Espera que ambas as tarefas de encaminhamento terminem
    tokio::try_join!(client_to_server_task, server_to_client_task)?;

    Ok(())
}

/// Transfere dados brutos entre duas metades de stream TCP.
async fn transfer_data(
    mut read_stream: tokio::net::tcp::OwnedReadHalf, // Recebe por valor
    mut write_stream: tokio::net::tcp::OwnedWriteHalf, // Recebe por valor
) -> Result<(), Error> {
    let mut buffer = [0; 32768]; // Buffer maior para melhor performance em transferências grandes
    loop {
        let bytes_read = read_stream.read(&mut buffer).await?;

        if bytes_read == 0 {
            break; // Conexão fechada
        }

        write_stream.write_all(&buffer[..bytes_read]).await?;
    }
    Ok(())
}

/// Espia o stream TCP para tentar detectar o protocolo (usado para SSH/OpenVPN).
async fn peek_stream(stream: &TcpStream) -> Result<String, Error> {
    let mut peek_buffer = vec![0; 32768]; // Buffer maior para espiar mais dados
    let bytes_peeked = stream.peek(&mut peek_buffer).await?;
    let data = &peek_buffer[..bytes_peeked];
    let data_str = String::from_utf8_lossy(data);
    Ok(data_str.to_string())
}

/// Obtém o valor de um argumento da linha de comando ou um valor padrão.
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

/// Obtém a porta para o proxy, da linha de comando ou padrão (80).
fn get_port() -> u16 {
    get_arg_value("--port", "80").parse().unwrap_or(80)
}

/// Obtém a string de status para o proxy, da linha de comando ou padrão ("@RustyManager").
fn get_status() -> String {
    get_arg_value("--status", "@RustyManager")
}
