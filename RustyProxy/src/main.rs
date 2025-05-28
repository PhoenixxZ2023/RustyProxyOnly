use std::env;
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::{timeout, Duration};

// Estrutura para gerenciar configurações
struct Config {
    port: u16,              // Porta única para todos os protocolos (via --port)
    status: String,         // Status para respostas HTTP
    ssh_port: u16,          // Porta do servidor SSH de backend
    openvpn_port: u16,      // Porta do servidor OpenVPN de backend
    websocket_port: u16,     // Porta do servidor WebSocket de backend
    timeout_secs: u64,      // Timeout para operações
}

impl Config {
    fn from_args() -> Self {
        Config {
            port: get_port(),
            status: get_status(),
            ssh_port: get_ssh_port(),
            openvpn_port: get_openvpn_port(),
            websocket_port: get_websocket_port(),
            timeout_secs: 1,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let config = Arc::new(Config::from_args());
    let listener = match TcpListener::bind(format!("[::]:{}", config.port)).await {
        Ok(listener) => listener,
        Err(e) => {
            println!("Erro ao vincular a porta {}: {}. Verifique se a porta está livre.", config.port, e);
            return Err(e);
        }
    };
    println!("Iniciando proxy na porta: {} para SSH, OpenVPN e WebSocket", config.port);
    start_proxy(listener, config).await;
    Ok(())
}

async fn start_proxy(listener: TcpListener, config: Arc<Config>) {
    let max_connections = Arc::new(Semaphore::new(1000));

    loop {
        let permit = max_connections.clone().acquire_owned().await;
        match listener.accept().await {
            Ok((client_stream, addr)) => {
                let config = config.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    println!("Nova conexão de {} na porta {}", addr, config.port);
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
        let protocol = detect_protocol(&client_stream, config).await?;
        let addr_proxy = match protocol {
            "ssh" => format!("0.0.0.0:{}", config.ssh_port),
            "openvpn" => format!("0.0.0.0:{}", config.openvpn_port),
            "websocket" => format!("0.0.0.0:{}", config.websocket_port),
            _ => format!("0.0.0.0:{}", config.ssh_port),
        };

        if protocol == "websocket" {
            // Envia resposta HTTP 101 inicial
            let mut client_stream = client_stream;
            client_stream
                .write_all(format!("HTTP/1.1 101 {}\r\n\r\n", config.status).as_bytes())
                .await?;

            // Realiza o handshake WebSocket
            let client_stream = perform_websocket_handshake(client_stream, config).await?;
            let server_stream = match TcpStream::connect(&addr_proxy).await {
                Ok(stream) => stream,
                Err(e) => {
                    println!("Erro ao conectar ao servidor WebSocket {}: {}. Verifique se o servidor está ativo.", addr_proxy, e);
                    return Err(e);
                }
            };
            let server_stream = perform_websocket_handshake(server_stream, config).await?;

            // Envia resposta HTTP 200 após handshake
            client_stream
                .write_all(format!("HTTP/1.1 200 {}\r\n\r\n", config.status).as_bytes())
                .await?;

            let (client_read, client_write) = client_stream.into_split();
            let (server_read, server_write) = server_stream.into_split();

            let client_read = Arc::new(Mutex::new(client_read));
            let client_write = Arc::new(Mutex::new(client_write));
            let server_read = Arc::new(Mutex::new(server_read));
            let server_write = Arc::new(Mutex::new(server_write));

            let client_to_server = transfer_websocket_data(client_read, server_write, config);
            let server_to_client = transfer_websocket_data(server_read, client_write, config);

            tokio::try_join!(client_to_server, server_to_client)?;
        } else {
            // Manipulação para SSH e OpenVPN
            let server_stream = match TcpStream::connect(&addr_proxy).await {
                Ok(stream) => stream,
                Err(e) => {
                    println!("Erro ao conectar ao servidor {}: {}. Verifique se o servidor está ativo.", addr_proxy, e);
                    return Err(e);
                }
            };

            let (client_read, client_write) = client_stream.into_split();
            let (server_read, server_write) = server_stream.into_split();

            let client_read = Arc::new(Mutex::new(client_read));
            let client_write = Arc::new(Mutex::new(client_write));
            let server_read = Arc::new(Mutex::new(server_read));
            let server_write = Arc::new(Mutex::new(server_write));

            let client_to_server = transfer_data(client_read, server_write, config);
            let server_to_client = transfer_data(server_read, client_write, config);

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

async fn perform_websocket_handshake(mut stream: TcpStream, config: &Config) -> Result<TcpStream, Error> {
    let mut buffer = vec![0; 32768];
    let bytes_read = timeout(Duration::from_secs(config.timeout_secs), stream.read(&mut buffer)).await
        .map_err(|_| Error::new(ErrorKind::TimedOut, "Timeout na leitura do handshake WebSocket"))??;
    let request = String::from_utf8_lossy(&buffer[..bytes_read]).to_string();
    println!("Handshake WebSocket recebido: {:?}", request);

    // Simplificação: aceita qualquer requisição WebSocket sem validar Sec-WebSocket-Key
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: simplified-accept-key\r\n\r\n"
    );

    timeout(Duration::from_secs(config.timeout_secs), stream.write_all(response.as_bytes())).await
        .map_err(|_| Error::new(ErrorKind::TimedOut, "Timeout na escrita do handshake WebSocket"))??;

    Ok(stream)
}

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
                Err(_) => return Err(Error::new(ErrorKind::TimedOut, "Timeout na leitura")),
            }
        };

        if bytes_read == 0 {
            println!("Conexão fechada, bytes lidos: 0");
            break;
        }

        let mut write_guard = write_stream.lock().await;
        match timeout(Duration::from_secs(config.timeout_secs), write_guard.write_all(&buffer[..bytes_read])).await {
            Ok(Ok(())) => println!("Transferidos {} bytes", bytes_read),
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(Error::new(ErrorKind::TimedOut, "Timeout na escrita")),
        }
    }
    Ok(())
}

async fn transfer_websocket_data(
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
                Err(_) => return Err(Error::new(ErrorKind::TimedOut, "Timeout na leitura WebSocket")),
            }
        };

        if bytes_read == 0 {
            println!("Conexão WebSocket fechada");
            break;
        }

        // Processar quadro WebSocket (simplificado: suporta apenas texto/binário, FIN=1, sem máscara)
        if bytes_read < 2 {
            return Err(Error::new(ErrorKind::InvalidData, "Quadro WebSocket inválido"));
        }
        let opcode = buffer[0] & 0x0F;
        let payload_len = buffer[1] & 0x7F;
        let payload_start = if payload_len <= 125 { 2 } else { 4 }; // Suporta comprimentos curtos
        if payload_start >= bytes_read {
            return Err(Error::new(ErrorKind::InvalidData, "Quadro WebSocket incompleto"));
        }
        let payload = &buffer[payload_start..bytes_read];

        if opcode != 0x1 && opcode != 0x2 {
            println!("Ignorando quadro WebSocket com opcode {}", opcode);
            continue; // Suporta apenas texto (0x1) e binário (0x2)
        }

        let mut write_guard = write_stream.lock().await;
        match timeout(Duration::from_secs(config.timeout_secs), write_guard.write_all(&buffer[..bytes_read])).await {
            Ok(Ok(())) => println!("Transferida mensagem WebSocket ({} bytes)", bytes_read),
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(Error::new(ErrorKind::TimedOut, "Timeout na escrita WebSocket")),
        }
    }
    Ok(())
}

async fn peek_stream(stream: &TcpStream) -> Result<String, Error> {
    let mut peek_buffer = vec![0; 32768]; // Aumentado para consistência
    let bytes_peeked = stream.peek(&mut peek_buffer).await?;
    let data = &peek_buffer[..bytes_peeked];
    let data_str = String::from_utf8_lossy(data).to_string();
    println!("Dados inspecionados: {:?}", data_str);
    Ok(data_str)
}

async fn detect_protocol(stream: &TcpStream, config: &Config) -> Result<&'static str, Error> {
    let data = timeout(Duration::from_secs(config.timeout_secs), peek_stream(stream))
        .await
        .unwrap_or_else(|_| Ok(String::new()))?;

    if data.starts_with("SSH-2.0-") {
        Ok("ssh")
    } else if data.contains("Upgrade: websocket") {
        Ok("websocket")
    } else if data.contains("HTTP/1.") {
        Ok("http")
    } else if data.is_empty() {
        println!("Nenhum dado recebido, assumindo SSH");
        Ok("ssh")
    } else {
        if data.len() >= 2 && data.as_bytes()[0] == 0x00 && data.as_bytes()[1] >= 0x14 {
            Ok("openvpn")
        } else {
            println!("Protocolo desconhecido, assumindo SSH");
            Ok("ssh")
        }
    }
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

fn get_status() -> String {
    let args: Vec<String> = env::args().collect();
    let mut status = String::from("@RustyManager");
    for i in 1..args.len() {
        if args[i] == "--status" && i + 1 < args.len() {
            status = args[i + 1].clone();
        }
    }
    status
}

fn get_ssh_port() -> u16 {
    let args: Vec<String> = env::args().collect();
    let mut port = 22;
    for i in 1..args.len() {
        if args[i] == "--ssh-port" && i + 1 < args.len() {
            port = args[i + 1].parse().unwrap_or(22);
        }
    }
    port
}

fn get_openvpn_port() -> u16 {
    let args: Vec<String> = env::args().collect();
    let mut port = 1194;
    for i in 1..args.len() {
        if args[i] == "--openvpn-port" && i + 1 < args.len() {
            port = args[i + 1].parse().unwrap_or(1194);
        }
    }
    port
}

fn get_websocket_port() -> u16 {
    let args: Vec<String> = env::args().collect();
    let mut port = 8081;
    for i in 1..args.len() {
        if args[i] == "--websocket-port" && i + 1 < args.len() {
            port = args[i + 1].parse().unwrap_or(8081);
        }
    }
    port
}
