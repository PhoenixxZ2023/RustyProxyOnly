use std::env;
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::{timeout, Duration};

// Importações para tokio-tungstenite
use tokio_tungstenite::{
    accept_async, connect_async,
    tungstenite::{
        handshake::client::Request,
        Message,
    },
};
use futures_util::StreamExt; // Para o método .next() em WebSocketStreamExt
use futures_util::SinkExt;   // Para o método .send() em WebSocketSinkExt

// Estrutura para gerenciar configurações
struct Config {
    port: u16,
    status: String,
    ssh_port: u16,
    openvpn_port: u16,
    websocket_port: u16, // Porta para WebSocket
    timeout_secs: u64,
}

impl Config {
    fn from_args() -> Self {
        Config {
            port: get_port(),
            status: get_status(),
            ssh_port: 22,
            openvpn_port: 1194,
            websocket_port: 8081, // Porta padrão para WebSocket
            timeout_secs: 1, // Timeout de 1 segundo para operações de leitura/escrita
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let config = Arc::new(Config::from_args());
    let listener = TcpListener::bind(format!("[::]:{}", config.port)).await?;
    println!("Iniciando serviço na porta: {}", config.port);
    println!("Porta SSH: {}", config.ssh_port);
    println!("Porta OpenVPN: {}", config.openvpn_port);
    println!("Porta WebSocket: {}", config.websocket_port);
    start_proxy(listener, config).await;
    Ok(())
}

async fn start_proxy(listener: TcpListener, config: Arc<Config>) {
    // Aumenta o limite para 10000 conexões simultâneas
    let max_connections = Arc::new(Semaphore::new(10000));

    loop {
        let permit = max_connections.clone().acquire_owned().await;
        match listener.accept().await {
            Ok((client_stream, addr)) => {
                let config = config.clone();
                tokio::spawn(async move {
                    let _permit = permit; // A permissão é liberada quando `_permit` sai do escopo
                    println!("Nova conexão de {}", addr);
                    if let Err(e) = handle_client(client_stream, &config).await {
                        println!("Erro ao processar cliente {}: {}", addr, e);
                    } else {
                        println!("Conexão com {} finalizada", addr);
                    }
                });
            }
            Err(e) => {
                println!("Erro ao aceitar conexão: {}", e);
            }
        }
    }
}

async fn handle_client(client_stream: TcpStream, config: &Config) -> Result<(), Error> {
    // Timeout para a manipulação completa do cliente
    let result = timeout(Duration::from_secs(30), async {
        let protocol = detect_protocol(&client_stream, config).await?;
        let addr_proxy = match protocol {
            "ssh" => format!("0.0.0.0:{}", config.ssh_port),
            "openvpn" => format!("0.0.0.0:{}", config.openvpn_port),
            "websocket" => format!("0.0.0.0:{}", config.websocket_port),
            _ => {
                println!("Protocolo desconhecido após detecção, encaminhando para SSH.");
                format!("0.0.0.0:{}", config.ssh_port)
            },
        };

        println!("Protocolo detectado: {}. Encaminhando para: {}", protocol, addr_proxy);

        if protocol == "websocket" {
            // Nova função para lidar com o proxy WebSocket
            handle_websocket_proxy(client_stream, &addr_proxy, config).await?;
        } else {
            // Manipulação para SSH e OpenVPN (ou qualquer outro que caia aqui)
            let server_stream = TcpStream::connect(&addr_proxy).await
                .map_err(|e| Error::new(ErrorKind::Other, format!("Erro ao conectar ao proxy {}: {}", addr_proxy, e)))?;

            let (client_read, client_write) = client_stream.into_split();
            let (server_read, server_write) = server_stream.into_split();

            let client_read_arc = Arc::new(Mutex::new(client_read));
            let client_write_arc = Arc::new(Mutex::new(client_write));
            let server_read_arc = Arc::new(Mutex::new(server_read));
            let server_write_arc = Arc::new(Mutex::new(server_write));

            let client_to_server = transfer_data(client_read_arc.clone(), server_write_arc.clone(), config);
            let server_to_client = transfer_data(server_read_arc.clone(), client_write_arc.clone(), config);

            tokio::try_join!(client_to_server, server_to_client)?;
        }
        Ok(())
    }).await;

    match result {
        Ok(res) => res,
        Err(e) => {
            println!("Timeout na manipulação do cliente: {}", e);
            Err(Error::new(ErrorKind::TimedOut, "Timeout na manipulação do cliente"))
        }
    }
}

// NOVO: Função para lidar com o proxy de WebSocket
async fn handle_websocket_proxy(
    client_stream: TcpStream,
    server_addr: &str,
    config: &Config,
) -> Result<(), Error> {
    println!("Iniciando proxy WebSocket para {}", server_addr);

    // 1. Aceita a conexão WebSocket do cliente (realiza o handshake completo)
    let ws_client_stream = accept_async(client_stream).await
        .map_err(|e| Error::new(ErrorKind::Other, format!("Falha no handshake WS do cliente: {}", e)))?;
    println!("Handshake WebSocket do cliente concluído.");

    // 2. Conecta ao servidor WebSocket de backend
    // Nota: Para WSS (WebSocket seguro), você precisaria de "wss://" e TLS.
    // Para simplificar, estamos usando ws:// para a conexão de backend.
    let (ws_server_stream, response) = connect_async(format!("ws://{}", server_addr)).await
        .map_err(|e| Error::new(ErrorKind::Other, format!("Falha ao conectar ao servidor WS de backend {}: {}", server_addr, e)))?;
    println!("Conectado ao servidor WebSocket de backend: {}. Resposta do servidor: {:?}", server_addr, response.status());


    // Separa as streams em sink (escrita) e stream (leitura)
    let (mut client_sink, mut client_stream) = ws_client_stream.split();
    let (mut server_sink, mut server_stream) = ws_server_stream.split();

    // Tarefa para encaminhar do cliente WS para o servidor WS
    let client_to_server_task = async move {
        loop {
            tokio::select! {
                // Tenta ler uma mensagem do cliente com timeout
                res = timeout(Duration::from_secs(config.timeout_secs), client_stream.next()) => {
                    let msg_opt = res.map_err(|_| Error::new(ErrorKind::TimedOut, "Timeout na leitura do cliente WS"))?;

                    let msg = match msg_opt {
                        Some(Ok(m)) => m,
                        Some(Err(e)) => {
                            println!("Erro ao ler mensagem do cliente WS: {}", e);
                            return Err(e.into());
                        },
                        None => {
                            println!("Cliente WS fechou a conexão.");
                            break; // Cliente fechou a stream
                        },
                    };

                    // Se for uma mensagem de fechamento, envia para o servidor e encerra
                    if msg.is_close() {
                        println!("Cliente WS enviou mensagem de CLOSE.");
                        server_sink.send(msg).await
                            .map_err(|e| Error::new(ErrorKind::Other, format!("Erro ao enviar CLOSE para servidor WS: {}", e)))?;
                        break;
                    }
                    if msg.is_ping() {
                        // tungstenite responde PONG automaticamente, apenas logamos
                        // println!("Cliente WS enviou PING.");
                        continue; // Não precisa encaminhar pings/pongs, a biblioteca cuida
                    }

                    // Encaminha a mensagem para o servidor
                    // println!("Encaminhando mensagem do cliente WS ({} bytes, tipo: {:?})", msg.len(), msg.opcode());
                    server_sink.send(msg).await
                        .map_err(|e| Error::new(ErrorKind::Other, format!("Erro ao enviar mensagem WS para servidor: {}", e)))?;
                }
            }
        }
        Ok::<(), Error>(()) // Especifica o tipo de retorno
    };

    // Tarefa para encaminhar do servidor WS para o cliente WS
    let server_to_client_task = async move {
        loop {
            tokio::select! {
                // Tenta ler uma mensagem do servidor com timeout
                res = timeout(Duration::from_secs(config.timeout_secs), server_stream.next()) => {
                    let msg_opt = res.map_err(|_| Error::new(ErrorKind::TimedOut, "Timeout na leitura do servidor WS"))?;

                    let msg = match msg_opt {
                        Some(Ok(m)) => m,
                        Some(Err(e)) => {
                            println!("Erro ao ler mensagem do servidor WS: {}", e);
                            return Err(e.into());
                        },
                        None => {
                            println!("Servidor WS fechou a conexão.");
                            break; // Servidor fechou a stream
                        },
                    };

                    if msg.is_close() {
                        println!("Servidor WS enviou mensagem de CLOSE.");
                        client_sink.send(msg).await
                            .map_err(|e| Error::new(ErrorKind::Other, format!("Erro ao enviar CLOSE para cliente WS: {}", e)))?;
                        break;
                    }
                    if msg.is_ping() {
                        // tungstenite responde PONG automaticamente
                        // println!("Servidor WS enviou PING.");
                        continue;
                    }

                    // Encaminha a mensagem para o cliente
                    // println!("Encaminhando mensagem do servidor WS ({} bytes, tipo: {:?})", msg.len(), msg.opcode());
                    client_sink.send(msg).await
                        .map_err(|e| Error::new(ErrorKind::Other, format!("Erro ao enviar mensagem WS para cliente: {}", e)))?;
                }
            }
        }
        Ok::<(), Error>(()) // Especifica o tipo de retorno
    };

    // Aguarda ambas as tarefas de encaminhamento terminarem
    tokio::try_join!(client_to_server_task, server_to_client_task)?;

    println!("Proxy WebSocket finalizado.");
    Ok(())
}


// --- Funções de transferência TCP genérica (não mudam) ---
async fn transfer_data(
    read_stream: Arc<Mutex<tokio::net::tcp::OwnedReadHalf>>,
    write_stream: Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
    config: &Config,
) -> Result<(), Error> {
    let mut buffer = vec![0; 32768];
    loop {
        let bytes_read = {
            let mut read_guard = read_stream.lock().await;
            match timeout(Duration::from_secs(config.timeout_secs), read_guard.read(&mut buffer)).await {
                Ok(Ok(n)) => n,
                Ok(Err(e)) => return Err(e),
                Err(_) => return Err(Error::new(ErrorKind::TimedOut, "Timeout na leitura TCP")),
            }
        };

        if bytes_read == 0 {
            // println!("Conexão TCP fechada, bytes lidos: 0");
            break;
        }

        let mut write_guard = write_stream.lock().await;
        match timeout(Duration::from_secs(config.timeout_secs), write_guard.write_all(&buffer[..bytes_read])).await {
            Ok(Ok(())) => { /* println!("Transferidos {} bytes", bytes_read); */ },
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(Error::new(ErrorKind::TimedOut, "Timeout na escrita TCP")),
        }
    }
    Ok(())
}

async fn peek_stream(stream: &TcpStream) -> Result<String, Error> {
    let mut peek_buffer = vec![0; 4096]; // Aumenta o buffer para inspeção
    let bytes_peeked = stream.peek(&mut peek_buffer).await?;
    let data = &peek_buffer[..bytes_peeked];
    let data_str = String::from_utf8_lossy(data).to_string();
    // println!("Dados inspecionados: {:?}", data_str);
    Ok(data_str)
}

// --- Funções de detecção de protocolo e args (não mudam, mas melhoradas) ---
async fn detect_protocol(stream: &TcpStream, config: &Config) -> Result<&'static str, Error> {
    let data = timeout(Duration::from_secs(config.timeout_secs * 2), peek_stream(stream)) // Aumenta o timeout para detecção
        .await
        .unwrap_or_else(|e| {
            println!("Timeout ou erro ao inspecionar stream: {}", e);
            Ok(String::new()) // Retorna string vazia em caso de timeout
        })?;

    // Priorize detecções mais específicas primeiro
    // WebSocket precisa de "Upgrade: websocket" E algum tipo de requisição HTTP (GET, POST, etc.)
    if data.contains("Upgrade: websocket") && (data.starts_with("GET ") || data.starts_with("POST ") || data.starts_with("CONNECT ")) {
        println!("Protocolo detectado: WebSocket (baseado em HTTP Upgrade)");
        Ok("websocket")
    } else if data.starts_with("SSH-2.0-") {
        println!("Protocolo detectado: SSH");
        Ok("ssh")
    } else if data.starts_with("CONNECT ") || data.starts_with("GET ") || data.starts_with("POST ") || data.starts_with("HTTP/1.") {
        println!("Protocolo detectado: HTTP genérico (pode ser túnel HTTP/HTTPS)");
        Ok("http") // Não é explicitamente um dos 3, mas é HTTP
    } else if data.len() >= 2 && data.as_bytes()[0] == 0x00 && data.as_bytes()[1] >= 0x14 {
        println!("Protocolo detectado: OpenVPN (baseado em bytes iniciais)");
        Ok("openvpn")
    } else if data.is_empty() {
        println!("Nenhum dado recebido durante a detecção, assumindo SSH (comum para clientes que enviam dados lentamente)");
        Ok("ssh")
    } else {
        println!("Protocolo desconhecido (dados iniciais: {:?}). Assumindo SSH.", &data);
        Ok("ssh")
    }
}

fn get_port() -> u16 {
    let args: Vec<String> = env::args().collect();
    let mut port = 80;
    for i in 1..args.len() {
        if args[i] == "--port" {
            if i + 1 < args.len() {
                port = args[i + 1].parse().unwrap_or_else(|_| {
                    eprintln!("Aviso: Porta inválida, usando 80.");
                    80
                });
            }
        }
    }
    port
}

fn get_status() -> String {
    let args: Vec<String> = env::args().collect();
    let mut status = String::from("@RustyManager");
    for i in 1..args.len() {
        if args[i] == "--status" {
            if i + 1 < args.len() {
                status = args[i + 1].clone();
            }
        }
    }
    status
}
