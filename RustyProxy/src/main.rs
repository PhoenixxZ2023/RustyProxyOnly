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
use futures_util::StreamExt;
use futures_util::SinkExt;

// Estrutura para gerenciar configurações
struct Config {
    port: u16,
    status: String,
    ssh_port: u16,
    openvpn_port: u16,
    websocket_port: u16,
    // stunnel_backend_port foi removido (não é mais passado para o RustyProxy)
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
            // stunnel_backend_port foi removido aqui também
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

async fn handle_client(mut client_stream: TcpStream, config: &Config) -> Result<(), Error> {
    let result = timeout(Duration::from_secs(30), async {
        // Fazer um peek inicial para detectar o protocolo
        let mut initial_buffer = vec![0; 4096];
        let bytes_peeked = client_stream.peek(&mut initial_buffer).await?;
        let initial_data_slice = &initial_buffer[..bytes_peeked]; // Slice para detecção
        let data_str = String::from_utf8_lossy(initial_data_slice);

        let mut protocol = "unknown";
        let mut addr_proxy = format!("0.0.0.0:{}", config.ssh_port); // Fallback padrão
        let mut is_http_connect_method = false; // Flag para método CONNECT

        // Lógica de detecção de protocolos, reordenada para prioridade
        if data_str.starts_with("CONNECT ") { // Detecção do método CONNECT
            let parts: Vec<&str> = data_str.split_whitespace().collect();
            if parts.len() >= 2 {
                let host_port_str = parts[1];
                // Tenta parsear host e porta do CONNECT
                if let Some((host, port_str)) = host_port_str.rsplit_once(':') {
                    if let Ok(port) = port_str.parse::<u16>() {
                        addr_proxy = format!("{}:{}", host, port);
                        protocol = "http_connect";
                        is_http_connect_method = true; // Seta a flag
                        println!("Protocolo detectado: HTTP CONNECT para {}. Encaminhando para: {}", host_port_str, addr_proxy);
                    }
                }
            }
        } else if data_str.contains("Upgrade: websocket") && 
                  (data_str.starts_with("GET ") || data_str.starts_with("POST ") || data_str.starts_with("CONNECT ")) {
            protocol = "websocket";
            addr_proxy = format!("0.0.0.0:{}", config.websocket_port);
            println!("Protocolo detectado: WebSocket. Encaminhando para: {}", addr_proxy);
        } else if data_str.starts_with("SSH-2.0-") {
            protocol = "ssh";
            addr_proxy = format!("0.0.0.0:{}", config.ssh_port);
            println!("Protocolo detectado: SSH. Encaminhando para: {}", addr_proxy);
        } else if initial_data_slice.len() >= 2 && initial_data_slice[0] == 0x00 && initial_data_slice[1] >= 0x14 {
            protocol = "openvpn";
            addr_proxy = format!("0.0.0.0:{}", config.openvpn_port);
            println!("Protocolo detectado: OpenVPN. Encaminhando para: {}", addr_proxy);
        } else if data_str.starts_with("GET ") || data_str.starts_with("POST ") || data_str.starts_with("HTTP/1.") {
            // Outras requisições HTTP (GET, POST etc.) que não são CONNECT nem WebSocket upgrade
            protocol = "http_other"; // Poderia ser um túnel HTTP ou outra coisa
            addr_proxy = format!("0.0.0.0:{}", config.ssh_port); // Fallback para SSH, ou outra porta se houver um HTTP backend
            println!("Protocolo detectado: HTTP (GET/POST/etc). Encaminhando para: {}", addr_proxy);
        } else if initial_data_slice.is_empty() {
            println!("Nenhum dado recebido, assumindo SSH.");
            protocol = "ssh";
            addr_proxy = format!("0.0.0.0:{}", config.ssh_port);
        } else {
            println!("Protocolo desconhecido (dados iniciais: {:?}). Assumindo SSH.", data_str);
            protocol = "ssh";
            addr_proxy = format!("0.0.0.0:{}", config.ssh_port);
        }

        // Lógica para HTTP CONNECT
        if is_http_connect_method {
            let mut server_stream = TcpStream::connect(&addr_proxy).await
                .map_err(|e| Error::new(ErrorKind::Other, format!("Erro ao conectar ao destino HTTP CONNECT {}: {}", addr_proxy, e)))?;

            let response = b"HTTP/1.1 200 Connection established\r\n\r\n";
            client_stream.write_all(response).await
                .map_err(|e| Error::new(ErrorKind::Other, format!("Erro ao enviar resposta HTTP CONNECT para o cliente: {}", e)))?;

            // NOVO: Consome a requisição CONNECT da stream do cliente.
            // A requisição CONNECT já foi detectada e seu conteúdo está no `initial_buffer`.
            // Agora precisamos "ler" esses bytes da stream real do cliente para que ela avance.
            let mut dummy_read_buffer = vec![0; bytes_peeked];
            client_stream.read_exact(&mut dummy_read_buffer).await?; 

            let (client_read, client_write) = client_stream.into_split();
            let (server_read, server_write) = server_stream.into_split();

            let client_read_arc = Arc::new(Mutex::new(client_read));
            let client_write_arc = Arc::new(Mutex::new(client_write));
            let server_read_arc = Arc::new(Mutex::new(server_read));
            let server_write_arc = Arc::new(Mutex::new(server_write));
            
            tokio::try_join!(client_to_server, server_to_client)?;

        } else if protocol == "websocket" {
            // Lógica para WebSocket
            handle_websocket_proxy(client_stream, &addr_proxy, config).await?;
        } else {
            // Lógica para SSH, OpenVPN, e outros TCP genéricos
            let mut server_stream = TcpStream::connect(&addr_proxy).await
                .map_err(|e| Error::new(ErrorKind::Other, format!("Erro ao conectar ao proxy {}: {}", addr_proxy, e)))?;

            // CORREÇÃO E0502: Clona os dados peeked e os escreve, depois consome do cliente
            let initial_data_to_send = initial_data_slice.to_vec(); // Cria uma CÓPIA dos dados
            
            // Consome os bytes que foram peeked da stream real do cliente para avançar o cursor
            let mut dummy_read_buffer = vec![0; bytes_peeked];
            client_stream.read_exact(&mut dummy_read_buffer).await?; 

            // Escreve a CÓPIA dos dados para o servidor de backend
            server_stream.write_all(&initial_data_to_send).await?; 

            // Separa as streams para transferência bidirecional
            let (client_read_half, client_write_half) = client_stream.into_split();
            let (server_read_half, server_write_half) = server_stream.into_split();

            let client_read_arc = Arc::new(Mutex::new(client_read_half));
            let client_write_arc = Arc::new(Mutex::new(client_write_half));
            let server_read_arc = Arc::new(Mutex::new(server_read_half));
            let server_write_arc = Arc::new(Mutex::new(server_write_half));
            
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

// handle_websocket_proxy (ajuste para split de streams)
async fn handle_websocket_proxy(
    mut client_stream: TcpStream, // Agora mutável para o split
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

    // CORREÇÃO: As metades de stream estavam invertidas no código anterior,
    // o que causaria dados do cliente indo para o cliente, e do servidor para o servidor.
    // Agora: client_sink é para ESCREVER no cliente, client_stream é para LER do cliente.
    // server_sink é para ESCREVER no servidor, server_stream é para LER do servidor.
    let (mut client_write_half, mut client_read_half) = ws_client_stream.split(); 
    let (mut server_write_half, mut server_read_half) = ws_server_stream.split(); 

    let client_to_server_task = async move {
        loop {
            tokio::select! {
                res = timeout(Duration::from_secs(config.timeout_secs), client_read_half.next()) => { // Lendo do cliente
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
                        server_write_half.send(msg).await // Escrevendo para o servidor
                            .map_err(|e| Error::new(ErrorKind::Other, format!("Erro ao enviar CLOSE para servidor WS: {}", e)))?;
                        break;
                    }
                    if msg.is_ping() { continue; }
                    server_write_half.send(msg).await // Escrevendo para o servidor
                        .map_err(|e| Error::new(ErrorKind::Other, format!("Erro ao enviar mensagem WS para servidor: {}", e)))?;
                }
            }
        }
        Ok::<(), Error>(())
    };

    let server_to_client_task = async move {
        loop {
            tokio::select! {
                res = timeout(Duration::from_secs(config.timeout_secs), server_read_half.next()) => { // Lendo do servidor
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
                        client_write_half.send(msg).await // Escrevendo para o cliente
                            .map_err(|e| Error::new(ErrorKind::Other, format!("Erro ao enviar CLOSE para cliente WS: {}", e)))?;
                        break;
                    }
                    if msg.is_ping() { continue; }
                    client_write_half.send(msg).await // Escrevendo para o cliente
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

// `peek_stream` permanece o mesmo (retorna Vec<u8>)
async fn peek_stream(stream: &TcpStream) -> Result<Vec<u8>, Error> {
    let mut peek_buffer = vec![0; 4096];
    let bytes_peeked = stream.peek(&mut peek_buffer).await?;
    Ok(peek_buffer[..bytes_peeked].to_vec())
}

// `detect_and_peek_protocol` e `get_stunnel_backend_port` foram removidos,
// pois a lógica foi consolidada ou não é mais necessária para este escopo.


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
