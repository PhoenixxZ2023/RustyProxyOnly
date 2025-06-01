use std::env;
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::{timeout, Duration};

// Estrutura para gerenciar configurações
#[derive(Debug, Clone)]
struct Config {
    port: u16,
    status: String,
    ssh_target_addr: String,
    openvpn_target_addr: String,
    http_target_addr: String,
    websocket_target_addr: String, // Novo para WebSocket
    peek_timeout_secs: u64,
    client_handling_timeout_secs: u64,
}

impl Config {
    fn from_args() -> Self {
        let args: Vec<String> = env::args().collect();
        let mut port = 80;
        let mut status = String::from("@RustyManager");
        let mut ssh_target_addr = String::from("0.0.0.0:22");
        let mut openvpn_target_addr = String::from("0.0.0.0:1194");
        let mut http_target_addr = String::from("0.0.0.0:8080");
        let mut websocket_target_addr = String::from("0.0.0.0:8081"); // Padrão para WebSocket backend
        let mut peek_timeout_secs = 2;
        let mut client_handling_timeout_secs = 30;

        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--port" => if i + 1 < args.len() { port = args[i + 1].parse().unwrap_or(port); i += 1; },
                "--status" => if i + 1 < args.len() { status = args[i + 1].clone(); i += 1; },
                "--ssh-target" => if i + 1 < args.len() { ssh_target_addr = args[i + 1].clone(); i += 1; },
                "--ovpn-target" => if i + 1 < args.len() { openvpn_target_addr = args[i + 1].clone(); i += 1; },
                "--http-target" => if i + 1 < args.len() { http_target_addr = args[i + 1].clone(); i += 1; },
                "--ws-target" => if i + 1 < args.len() { websocket_target_addr = args[i + 1].clone(); i += 1; }, // Novo arg
                "--peek-timeout" => if i + 1 < args.len() { peek_timeout_secs = args[i + 1].parse().unwrap_or(peek_timeout_secs); i += 1; },
                "--client-timeout" => if i + 1 < args.len() { client_handling_timeout_secs = args[i+1].parse().unwrap_or(client_handling_timeout_secs); i += 1; },
                _ => {}
            }
            i += 1;
        }
        Config { port, status, ssh_target_addr, openvpn_target_addr, http_target_addr, websocket_target_addr, peek_timeout_secs, client_handling_timeout_secs }
    }
}

// Enum para o tipo de requisição detectada
#[derive(Debug)]
enum DetectedRequestType {
    Ssh,
    OpenVpn,
    HttpConnect { host: String, port: u16 },
    HttpPlain,
    WebSocketUpgrade, // Nova variante
    Unknown,
}


#[tokio::main]
async fn main() -> Result<(), Error> {
    let config = Arc::new(Config::from_args());
    println!("Configurações carregadas: {:?}", config);
    let listener = TcpListener::bind(format!("[::]:{}", config.port)).await?;
    println!("Iniciando serviço na porta: {}", config.port);
    start_http(listener, config).await;
    Ok(())
}

async fn start_http(listener: TcpListener, config: Arc<Config>) {
    let max_connections = Arc::new(Semaphore::new(1000));
    loop {
        let permit = match max_connections.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => { println!("Erro ao adquirir permissão do semáforo. Encerrando."); return; }
        };
        match listener.accept().await {
            Ok((client_stream, addr)) => {
                let config_clone = config.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(e) = handle_client(client_stream, config_clone).await {
                        println!("Erro ao processar cliente {}: {}", addr, e);
                    }
                });
            }
            Err(e) => { println!("Erro ao aceitar conexão: {}", e); }
        }
    }
}

async fn handle_client(mut client_stream: TcpStream, config: Arc<Config>) -> Result<(), Error> {
    let result = timeout(Duration::from_secs(config.client_handling_timeout_secs), async {
        // Realizar o peek dos dados iniciais do cliente aqui
        let mut peek_buffer = vec![0; 2048]; // Buffer para peek
        let bytes_peeked = match timeout(Duration::from_secs(config.peek_timeout_secs), client_stream.peek(&mut peek_buffer)).await {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                println!("Erro durante o peek inicial do cliente: {}", e);
                return Err(e);
            }
            Err(_) => { // Timeout durante o peek
                println!("Timeout durante o peek inicial do cliente ({}s).", config.peek_timeout_secs);
                return Err(Error::new(ErrorKind::TimedOut, "Timeout no peek inicial"));
            }
        };

        if bytes_peeked == 0 {
            println!("Nenhum dado recebido do cliente no peek inicial. Fechando conexão.");
            return Ok(()); // Cliente desconectou ou não enviou dados
        }
        let actual_peek_data = &peek_buffer[..bytes_peeked];

        // Chamar detect_protocol com os dados "espiados"
        // detect_protocol agora é síncrona, pois só analisa o buffer
        let detected_type = detect_protocol(actual_peek_data);

        println!("Tipo de requisição detectada: {:?}", detected_type);

        match detected_type {
            DetectedRequestType::HttpConnect { host, port } => {
                let target_connect_addr = format!("{}:{}", host, port);
                println!("Processando CONNECT para: {}", target_connect_addr);
                match TcpStream::connect(&target_connect_addr).await {
                    Ok(server_stream) => {
                        client_stream.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await?;
                        pipe_streams(client_stream, server_stream).await
                    }
                    Err(e) => {
                        eprintln!("Falha ao conectar ao destino do CONNECT {}: {}", target_connect_addr, e);
                        let response = b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                        client_stream.write_all(response).await.unwrap_or_else(|write_err| {
                            println!("Erro ao enviar resposta 502 ao cliente: {}", write_err);
                        });
                        Err(e)
                    }
                }
            }
            DetectedRequestType::WebSocketUpgrade => {
                println!("Processando WebSocket Upgrade para: {}", config.websocket_target_addr);
                match TcpStream::connect(&config.websocket_target_addr).await {
                    Ok(mut server_stream) => {
                        // Enviar a requisição original de upgrade (peek_data) para o servidor WebSocket backend
                        server_stream.write_all(actual_peek_data).await?;
                        // Agora o backend WebSocket responderá com 101 Switching Protocols (ou erro)
                        // e então pipe_streams encaminhará tudo.
                        pipe_streams(client_stream, server_stream).await
                    }
                    Err(e) => {
                        eprintln!("Falha ao conectar ao backend WebSocket {}: {}", config.websocket_target_addr, e);
                        // Poderia enviar um erro HTTP 50x ao cliente aqui também, se apropriado
                        // antes de fechar a conexão.
                        let response = b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                        client_stream.write_all(response).await.unwrap_or_else(|write_err| {
                            println!("Erro ao enviar resposta 503 ao cliente: {}", write_err);
                        });
                        Err(e)
                    }
                }
            }
            DetectedRequestType::Ssh => {
                println!("Redirecionando para SSH: {}", config.ssh_target_addr);
                let server_stream = TcpStream::connect(&config.ssh_target_addr).await?;
                pipe_streams(client_stream, server_stream).await
            }
            DetectedRequestType::OpenVpn => {
                println!("Redirecionando para OpenVPN/Default: {}", config.openvpn_target_addr);
                let server_stream = TcpStream::connect(&config.openvpn_target_addr).await?;
                pipe_streams(client_stream, server_stream).await
            }
            DetectedRequestType::HttpPlain => {
                println!("Redirecionando para HTTP Plain: {}", config.http_target_addr);
                let mut server_stream = TcpStream::connect(&config.http_target_addr).await?;
                // Enviar os dados já "espiados" para o servidor HTTP, pois ele espera a requisição completa
                server_stream.write_all(actual_peek_data).await?;
                pipe_streams(client_stream, server_stream).await
            }
            DetectedRequestType::Unknown => {
                 println!("Tipo de requisição desconhecido. Fechando conexão.");
                 Err(Error::new(ErrorKind::InvalidData, "Tipo de requisição desconhecido"))
            }
        }
    }).await;

    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(_elapsed_error) => {
            println!("Timeout de {}s na manipulação do cliente", config.client_handling_timeout_secs);
            Err(Error::new(ErrorKind::TimedOut, "Timeout na manipulação do cliente"))
        }
    }
}

async fn pipe_streams(client_stream: TcpStream, server_stream: TcpStream) -> Result<(), Error> {
    let (client_read, client_write) = client_stream.into_split();
    let (server_read, server_write) = server_stream.into_split();

    let client_read_arc = Arc::new(Mutex::new(client_read));
    let client_write_arc = Arc::new(Mutex::new(client_write));
    let server_read_arc = Arc::new(Mutex::new(server_read));
    let server_write_arc = Arc::new(Mutex::new(server_write));

    let client_to_server = transfer_data(client_read_arc, server_write_arc.clone());
    let server_to_client = transfer_data(server_read_arc, client_write_arc.clone());

    tokio::try_join!(client_to_server, server_to_client)?;
    Ok(())
}

async fn transfer_data(
    read_stream_arc: Arc<Mutex<tokio::net::tcp::OwnedReadHalf>>,
    write_stream_arc: Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
) -> Result<(), Error> {
    let mut buffer = [0; 8192];
    loop {
        let bytes_read = {
            let mut read_guard = read_stream_arc.lock().await;
            read_guard.read(&mut buffer).await?
        };
        if bytes_read == 0 { break; }
        let mut write_guard = write_stream_arc.lock().await;
        write_guard.write_all(&buffer[..bytes_read]).await?;
    }
    Ok(())
}

// detect_protocol agora é síncrona e apenas analisa o buffer fornecido
fn detect_protocol(peek_data: &[u8]) -> DetectedRequestType {
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut req = httparse::Request::new(&mut headers);

    match req.parse(peek_data) {
        Ok(httparse::Status::Complete(_)) | Ok(httparse::Status::Partial) => {
            if req.method == Some("CONNECT") {
                if let Some(path) = req.path {
                    let parts: Vec<&str> = path.split(':').collect();
                    if parts.len() == 2 {
                        if let Ok(port) = parts[1].parse::<u16>() {
                            return DetectedRequestType::HttpConnect { host: parts[0].to_string(), port };
                        } else {
                            println!("Porta inválida no CONNECT: {}", path);
                            return DetectedRequestType::Unknown;
                        }
                    } else {
                        println!("Formato inválido de host:porta no CONNECT: {}", path);
                        return DetectedRequestType::Unknown;
                    }
                } else {
                     println!("CONNECT sem path.");
                     return DetectedRequestType::Unknown;
                }
            } else if req.method.is_some() { // Se tem um método, é algum tipo de HTTP
                // Verificar por upgrade WebSocket
                let mut is_websocket_upgrade = false;
                let mut connection_has_upgrade = false;
                for header in req.headers.iter() {
                    if header.name.eq_ignore_ascii_case("Upgrade") {
                        if String::from_utf8_lossy(header.value).eq_ignore_ascii_case("websocket") {
                            is_websocket_upgrade = true;
                        }
                    } else if header.name.eq_ignore_ascii_case("Connection") {
                        // Connection header pode ter múltiplos valores, ex: "keep-alive, Upgrade"
                        if String::from_utf8_lossy(header.value)
                            .split(',')
                            .any(|val| val.trim().eq_ignore_ascii_case("Upgrade"))
                        {
                            connection_has_upgrade = true;
                        }
                    }
                }

                if is_websocket_upgrade && connection_has_upgrade {
                    return DetectedRequestType::WebSocketUpgrade;
                } else {
                    return DetectedRequestType::HttpPlain; // Outro método HTTP
                }
            } else {
                // Parcial, mas sem método ainda. Pode ser SSH ou OpenVPN.
            }
        }
        Err(_e) => {
            // Não é HTTP, ou é malformado. Continuar para outras verificações.
            // println!("Erro ao parsear HTTP (pode ser outro protocolo): {:?}", e);
        }
    }

    if peek_data.len() > 4 && peek_data.starts_with(b"SSH-") {
        return DetectedRequestType::Ssh;
    }
    
    // Fallback se não for HTTP nem SSH
    // println!("Não detectado como HTTP ou SSH, assumindo OpenVPN/Default.");
    DetectedRequestType::OpenVpn
}
