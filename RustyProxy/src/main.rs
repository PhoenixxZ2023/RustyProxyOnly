use std::env;
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::{timeout, Duration};

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
                // Caso fallback, o detect_protocol já deve ter assumido ssh,
                // mas para garantir, manteremos um fallback explícito.
                println!("Protocolo desconhecido após detecção, encaminhando para SSH.");
                format!("0.0.0.0:{}", config.ssh_port)
            },
        };

        println!("Protocolo detectado: {}. Encaminhando para: {}", protocol, addr_proxy);

        if protocol == "websocket" {
            // Realiza o handshake WebSocket manualmente no lado do cliente
            let client_stream = perform_websocket_handshake(client_stream, config).await?;
            let server_stream = TcpStream::connect(&addr_proxy).await
                .map_err(|e| Error::new(ErrorKind::Other, format!("Erro ao conectar ao proxy WebSocket {}: {}", addr_proxy, e)))?;
            // Realiza o handshake WebSocket manualmente no lado do servidor (se o servidor for um endpoint WebSocket)
            let server_stream = perform_websocket_handshake(server_stream, config).await?;

            let (client_read, client_write) = client_stream.into_split();
            let (server_read, server_write) = server_stream.into_split();

            let client_read_arc = Arc::new(Mutex::new(client_read));
            let client_write_arc = Arc::new(Mutex::new(client_write));
            let server_read_arc = Arc::new(Mutex::new(server_read));
            let server_write_arc = Arc::new(Mutex::new(server_write));

            let client_to_server = transfer_websocket_data(client_read_arc.clone(), server_write_arc.clone(), config);
            let server_to_client = transfer_websocket_data(server_read_arc.clone(), client_write_arc.clone(), config);

            tokio::try_join!(client_to_server, server_to_client)?;
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

async fn perform_websocket_handshake(mut stream: TcpStream, config: &Config) -> Result<TcpStream, Error> {
    let mut buffer = vec![0; 4096]; // Buffer menor para o handshake, já que não é esperado um request gigante
    let bytes_read = timeout(Duration::from_secs(config.timeout_secs), stream.read(&mut buffer)).await
        .map_err(|_| Error::new(ErrorKind::TimedOut, "Timeout na leitura do handshake WebSocket"))??;
    let request = String::from_utf8_lossy(&buffer[..bytes_read]).to_string();
    println!("Handshake WebSocket recebido: {:?}", request);

    // Simplificação: aceita qualquer requisição WebSocket sem validar Sec-WebSocket-Key
    // Em um ambiente de produção, você precisaria de uma validação completa
    // e da geração da resposta Sec-WebSocket-Accept correta.
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: simplified-accept-key\r\n\r\n" // Esta chave é fixa, não segura para prod.
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
            // println!("Conexão TCP fechada, bytes lidos: 0"); // Pode ser muito log
            break;
        }

        let mut write_guard = write_stream.lock().await;
        match timeout(Duration::from_secs(config.timeout_secs), write_guard.write_all(&buffer[..bytes_read])).await {
            Ok(Ok(())) => { /* println!("Transferidos {} bytes", bytes_read); */ }, // Pode ser muito log
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
            // println!("Conexão WebSocket fechada"); // Pode ser muito log
            break;
        }

        // Processar quadro WebSocket (simplificado: suporta apenas texto/binário, FIN=1, sem máscara)
        if bytes_read < 2 {
            return Err(Error::new(ErrorKind::InvalidData, "Quadro WebSocket inválido ou muito curto"));
        }
        let opcode = buffer[0] & 0x0F;
        let fin_rsv_mask = buffer[0] & 0xF0; // FIN, RSV1, RSV2, RSV3

        // Apenas para log, payload_len e payload_start são mais complexos com extensões e chaves de máscara.
        // Como o proxy está apenas encaminhando, não precisa desmascarar ou interpretar o payload aqui.
        // A lógica de transferência apenas retransmite os bytes brutos do quadro WebSocket.
        // A detecção já deveria ter garantido que o fluxo é WebSocket.

        if opcode != 0x1 && opcode != 0x2 && opcode != 0x8 && opcode != 0x9 && opcode != 0xA {
            // Log apenas opcodes que não são de dados ou de controle (ping, pong, close)
            // println!("Ignorando/repassando quadro WebSocket com opcode desconhecido {}", opcode);
        }

        let mut write_guard = write_stream.lock().await;
        match timeout(Duration::from_secs(config.timeout_secs), write_guard.write_all(&buffer[..bytes_read])).await {
            Ok(Ok(())) => { /* println!("Transferida mensagem WebSocket ({} bytes)", bytes_read); */ }, // Pode ser muito log
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(Error::new(ErrorKind::TimedOut, "Timeout na escrita WebSocket")),
        }
    }
    Ok(())
}

async fn peek_stream(stream: &TcpStream) -> Result<String, Error> {
    let mut peek_buffer = vec![0; 4096]; // Aumenta o buffer para inspeção
    let bytes_peeked = stream.peek(&mut peek_buffer).await?;
    let data = &peek_buffer[..bytes_peeked];
    let data_str = String::from_utf8_lossy(data).to_string();
    // println!("Dados inspecionados: {:?}", data_str); // Pode ser muito log
    Ok(data_str)
}

async fn detect_protocol(stream: &TcpStream, config: &Config) -> Result<&'static str, Error> {
    let data = timeout(Duration::from_secs(config.timeout_secs * 2), peek_stream(stream)) // Aumenta o timeout para detecção
        .await
        .unwrap_or_else(|e| {
            println!("Timeout ou erro ao inspecionar stream: {}", e);
            Ok(String::new()) // Retorna string vazia em caso de timeout
        })?;

    // Priorize detecções mais específicas primeiro
    if data.contains("Upgrade: websocket") && data.contains("GET / HTTP/1.1") {
        println!("Protocolo detectado: WebSocket (baseado em HTTP Upgrade)");
        Ok("websocket")
    } else if data.starts_with("SSH-2.0-") {
        println!("Protocolo detectado: SSH");
        Ok("ssh")
    } else if data.starts_with("CONNECT") || data.starts_with("GET ") || data.starts_with("POST ") {
        println!("Protocolo detectado: HTTP (requisição HTTP inicial, pode ser túnel)");
        Ok("http") // Pode ser um HTTP proxy ou túnel, que eventualmente pode ser WebSocket
    } else if data.len() >= 2 && data.as_bytes()[0] == 0x00 && data.as_bytes()[1] >= 0x14 {
        // Esta é uma heurística para OpenVPN. É importante entender que não é 100% garantido.
        println!("Protocolo detectado: OpenVPN (baseado em bytes iniciais)");
        Ok("openvpn")
    } else if data.is_empty() {
        println!("Nenhum dado recebido durante a detecção, assumindo SSH (comum para clientes que enviam dados lentamente)");
        Ok("ssh") // Fallback para SSH se nenhum dado for recebido rapidamente
    } else {
        // Se nada for detectado, loga o que foi visto e assume SSH
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
