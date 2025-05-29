use std::env;
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::{timeout, Duration};

// Importações para tokio-tungstenite
use tokio_tungstenite::{
    accept_hdr_async, connect_async,
    tungstenite::{
        handshake::server::{Request, Response},
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
    websocket_port: u16,
    stunnel_backend_port: u16, // <--- PORTA DE BACKEND PARA STUNNEL
    timeout_secs: u64,
}

impl Config {
    fn from_args() -> Self {
        Config {
            port: get_port(),
            status: get_status(),
            ssh_port: 22,
            openvpn_port: 1194,
            websocket_port: 8081,
            stunnel_backend_port: 444, // <--- Padrão para backend Stunnel
            timeout_secs: 1,
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
    println!("Porta de Backend Stunnel: {}", config.stunnel_backend_port); // <--- Log da porta Stunnel
    start_proxy(listener, config).await;
    Ok(())
}

async fn start_proxy(listener: TcpListener, config: Arc<Config>) {
    let max_connections = Arc::new(Semaphore::new(10000));

    loop {
        let permit = max_connections.clone().acquire_owned().await;
        match listener.accept().await {
            Ok((client_stream, addr)) => {
                let config = config.clone();
                tokio::spawn(async move {
                    let _permit = permit;
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
    let result = timeout(Duration::from_secs(30), async {
        let (protocol, initial_data) = detect_and_peek_protocol(&client_stream, config).await?;
        
        let addr_proxy = match protocol {
            "ssh" => format!("0.0.0.0:{}", config.ssh_port),
            "openvpn" => format!("0.0.0.0:{}", config.openvpn_port),
            "websocket" => format!("0.0.0.0:{}", config.websocket_port),
            "stunnel" => format!("0.0.0.0:{}", config.stunnel_backend_port), // <--- Roteamento para Stunnel
            _ => { // Fallback
                println!("Protocolo desconhecido ou fallback, encaminhando para SSH.");
                format!("0.0.0.0:{}", config.ssh_port)
            },
        };

        println!("Protocolo detectado: {}. Encaminhando para: {}", protocol, addr_proxy);

        if protocol == "websocket" {
            handle_websocket_proxy(client_stream, &addr_proxy, config).await?;
        } else {
            // Para SSH, OpenVPN, Stunnel e outros TCP genéricos
            let server_stream = TcpStream::connect(&addr_proxy).await
                .map_err(|e| Error::new(ErrorKind::Other, format!("Erro ao conectar ao proxy {}: {}", addr_proxy, e)))?;

            let (mut client_read_half, client_write_half) = client_stream.into_split();
            let (server_read_half, mut server_write_half) = server_stream.into_split();

            // Escreve os dados iniciais que foram 'peeked' para o servidor de backend
            server_write_half.write_all(&initial_data).await?; 

            let client_read_arc = Arc::new(Mutex::new(client_read_half));
            let client_write_arc = Arc::new(Mutex::new(client_write_half));
            let server_read_arc = Arc::new(Mutex::new(server_read_half));
            let server_write_arc = Arc::new(Mutex::new(server_write_half));
            
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

// Função para lidar com o proxy de WebSocket
async fn handle_websocket_proxy(
    client_stream: TcpStream,
    server_addr: &str,
    config: &Config,
) -> Result<(), Error> {
    println!("Iniciando proxy WebSocket para {}", server_addr);

    let ws_client_stream = accept_hdr_async(client_stream, |req: &Request, mut response: Response| {
        println!("Handshake Request Headers do Cliente: {:?}", req.headers());
        if let Some(user_agent) = req.headers().get("User-Agent") {
            println!("User-Agent do Cliente: {:?}", user_agent);
        }
        Ok(response)
    }).await
    .map_err(|e| Error::new(ErrorKind::Other, format!("Falha no handshake WS do cliente: {}", e)))?;
    println!("Handshake WebSocket do cliente concluído.");

    let (ws_server_stream, response_server) = connect_async(format!("ws://{}", server_addr)).await
        .map_err(|e| Error::new(ErrorKind::Other, format!("Falha ao conectar ao servidor WS de backend {}: {}", server_addr, e)))?;
    println!("Conectado ao servidor WebSocket de backend: {}. Resposta do servidor: {:?}", server_addr, response_server.status());

    let (mut client_sink, mut client_stream) = ws_client_stream.split();
    let (mut server_sink, mut server_stream) = ws_server_stream.split();

    let client_to_server_task = async move {
        loop {
            tokio::select! {
                res = timeout(Duration::from_secs(config.timeout_secs), client_stream.next()) => {
                    let msg_opt = res.map_err(|_| Error::new(ErrorKind::TimedOut, "Timeout na leitura do cliente WS"))?;
                    let msg = match msg_opt {
                        Some(Ok(m)) => m,
                        Some(Err(e)) => {
                            return Err(Error::new(ErrorKind::Other, format!("Erro ao ler mensagem do cliente WS: {}", e)));
                        },
                        None => {
                            println!("Cliente WS fechou a conexão.");
                            break;
                        },
                    };
                    if msg.is_close() {
                        println!("Cliente WS enviou mensagem de CLOSE.");
                        server_sink.send(msg).await
                            .map_err(|e| Error::new(ErrorKind::Other, format!("Erro ao enviar CLOSE para servidor WS: {}", e)))?;
                        break;
                    }
                    if msg.is_ping() { continue; }
                    server_sink.send(msg).await
                        .map_err(|e| Error::new(ErrorKind::Other, format!("Erro ao enviar mensagem WS para servidor: {}", e)))?;
                }
            }
        }
        Ok::<(), Error>(())
    };

    let server_to_client_task = async move {
        loop {
            tokio::select! {
                res = timeout(Duration::from_secs(config.timeout_secs), server_stream.next()) => {
                    let msg_opt = res.map_err(|_| Error::new(ErrorKind::TimedOut, "Timeout na leitura do servidor WS"))?;
                    let msg = match msg_opt {
                        Some(Ok(m)) => m,
                        Some(Err(e)) => {
                            return Err(Error::new(ErrorKind::Other, format!("Erro ao ler mensagem do servidor WS: {}", e)));
                        },
                        None => {
                            println!("Servidor WS fechou a conexão.");
                            break;
                        },
                    };
                    if msg.is_close() {
                        println!("Servidor WS enviou mensagem de CLOSE.");
                        client_sink.send(msg).await
                            .map_err(|e| Error::new(ErrorKind::Other, format!("Erro ao enviar CLOSE para cliente WS: {}", e)))?;
                        break;
                    }
                    if msg.is_ping() { continue; }
                    client_sink.send(msg).await
                        .map_err(|e| Error::new(ErrorKind::Other, format!("Erro ao enviar mensagem WS para cliente: {}", e)))?;
                }
            }
        }
        Ok::<(), Error>(())
    };

    tokio::try_join!(client_to_server_task, server_to_client_task)?;

    println!("Proxy WebSocket finalizado.");
    Ok(())
}


// --- Funções de transferência TCP genérica ---
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

async fn peek_stream(stream: &TcpStream) -> Result<Vec<u8>, Error> { // <--- Retorna Vec<u8> direto
    let mut peek_buffer = vec![0; 4096];
    let bytes_peeked = stream.peek(&mut peek_buffer).await?;
    Ok(peek_buffer[..bytes_peeked].to_vec()) // <--- Retorna os bytes diretamente
}

// <--- detect_and_peek_protocol: Retorna o protocolo e os dados que foram peeked
async fn detect_and_peek_protocol(stream: &TcpStream, config: &Config) -> Result<(&'static str, Vec<u8>), Error> {
    let initial_data = timeout(Duration::from_secs(config.timeout_secs * 2), peek_stream(stream))
        .await
        .unwrap_or_else(|e| {
            println!("Timeout ou erro ao inspecionar stream para detecção: {}", e);
            Ok(Vec::new()) // Retorna vetor vazio em caso de timeout
        })?;

    // As verificações são feitas com base nos bytes, não na string lossy, para maior precisão
    // E a ordem é importante para evitar detecções incorretas

    // 1. TLS (Stunnel) - Verifica bytes iniciais de um handshake TLS
    // Um handshake TLS começa com Record Type 0x16 (Handshake) e versão TLS (0x03 0xXX)
    // O byte 0x03 é comum a TLS 1.0, 1.1, 1.2, 1.3
    if initial_data.len() >= 5 && initial_data[0] == 0x16 && initial_data[1] == 0x03 {
        println!("Protocolo detectado: TLS (Stunnel)");
        return Ok(("stunnel", initial_data));
    }
    // 2. WebSocket (requer cabeçalhos HTTP específicos)
    // Converte para string apenas para a detecção de padrões textuais
    let data_str = String::from_utf8_lossy(&initial_data);
    if data_str.contains("Upgrade: websocket") && 
       (data_str.starts_with("GET ") || data_str.starts_with("POST ") || data_str.starts_with("CONNECT ")) {
        println!("Protocolo detectado: WebSocket (baseado em HTTP Upgrade)");
        return Ok(("websocket", initial_data));
    }
    // 3. SSH (assinatura muito clara)
    else if data_str.starts_with("SSH-2.0-") {
        println!("Protocolo detectado: SSH");
        return Ok(("ssh", initial_data));
    }
    // 4. OpenVPN (heurística baseada em bytes iniciais)
    else if initial_data.len() >= 2 && initial_data[0] == 0x00 && initial_data[1] >= 0x14 {
        println!("Protocolo detectado: OpenVPN (baseado em bytes iniciais)");
        return Ok(("openvpn", initial_data));
    }
    // 5. HTTP genérico (pode ser túnel HTTP/HTTPS que não é WebSocket)
    else if data_str.starts_with("CONNECT ") || data_str.starts_with("GET ") || data_str.starts_with("POST ") || data_str.starts_with("HTTP/1.") {
        println!("Protocolo detectado: HTTP genérico (pode ser túnel HTTP/HTTPS)");
        return Ok(("http", initial_data));
    }
    // 6. Fallback (se nenhum dado ou padrão claro for encontrado)
    else if initial_data.is_empty() {
        println!("Nenhum dado recebido, assumindo SSH (comum para clientes que enviam dados lentamente)");
        return Ok(("ssh", initial_data));
    } else {
        println!("Protocolo desconhecido (dados iniciais: {:?}). Assumindo SSH.", data_str);
        return Ok(("ssh", initial_data));
    }
}


fn get_port() -> u16 {
    let args: Vec<String> = env::args().collect();
    let mut port = 80;
    // Permite que a porta principal do proxy seja definida via --port
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

// NOVO: Função para obter a porta de backend do Stunnel dos argumentos
fn get_stunnel_backend_port() -> u16 {
    let args: Vec<String> = env::args().collect();
    let mut stunnel_port = 444; // Padrão se não for fornecido
    for i in 1..args.len() {
        if args[i] == "--stunnel_backend_port" {
            if i + 1 < args.len() {
                stunnel_port = args[i + 1].parse().unwrap_or_else(|_| {
                    eprintln!("Aviso: Porta de backend Stunnel inválida, usando 444.");
                    444
                });
            }
        }
    }
    stunnel_port
}


fn get_status() -> String {
    let args: Vec<String> = env::args().collect();
    let mut status = String::from("@RustyManager");
    for i in 1..args.len() {
        if i + 1 < args.len() && args[i] == "--status" {
            status = args[i + 1].clone();
        }
    }
    status
}
