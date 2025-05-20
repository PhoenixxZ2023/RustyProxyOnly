use std::env;
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::{timeout, Duration};
use std::collections::HashMap;

// Estrutura para gerenciar configurações
struct Config {
    port: u16,
    status: String,
    ssh_port: u16,
    openvpn_port: u16,
    udp_port: u16, // Porta fixa para UDP
    timeout_secs: u64,
    default_udp_host: String, // Destino padrão para UDP
}

impl Config {
    fn from_args() -> Self {
        Config {
            port: get_port(),
            status: get_status(),
            ssh_port: 22,
            openvpn_port: 1194,
            udp_port: 7300, // Porta fixa para escutar UDP
            timeout_secs: 1,
            default_udp_host: "0.0.0.0:7300".to_string(), // Destino padrão para UDP
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    // Iniciando o proxy com configurações
    let config = Config::from_args();
    let listener = TcpListener::bind(format!("[::]:{}", config.port)).await?;
    let udp_socket = UdpSocket::bind(format!("[::]:{}", config.udp_port)).await?;
    
    println!("Iniciando serviço TCP na porta: {}", config.port);
    println!("Iniciando serviço UDP na porta: {}", config.udp_port);
    println!("RustyManager - Proxy TCP & UDP");
    
    // Mapa para armazenar destinos UDP por cliente
    let udp_targets = Arc::new(Mutex::new(HashMap::new()));
    
    // Iniciar servidores TCP e UDP em paralelo
    tokio::join!(
        start_http(listener, udp_targets.clone()),
        start_udp(udp_socket, udp_targets, config.default_udp_host.clone())
    );
    
    Ok(())
}

async fn start_http(listener: TcpListener, udp_targets: Arc<Mutex<HashMap<String, (String, u16)>>>) {
    // Adiciona máximo de conexões simultâneas
    let max_connections = Arc::new(Semaphore::new(1000));

    loop {
        let permit = max_connections.clone().acquire_owned().await;
        match listener.accept().await {
            Ok((client_stream, addr)) => {
                let udp_targets = udp_targets.clone();
                tokio::spawn(async move {
                    let _permit = permit; // Mantém o permit ativo
                    if let Err(e) = handle_client(client_stream, addr, udp_targets).await {
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

async fn start_udp(socket: UdpSocket, udp_targets: Arc<Mutex<HashMap<String, (String, u16)>>>, default_udp_host: String) {
    let socket = Arc::new(socket);
    let mut buffer = vec![0; 8192];

    loop {
        match socket.recv_from(&mut buffer).await {
            Ok((bytes_read, client_addr)) => {
                let data = &buffer[..bytes_read];
                let client_addr_str = client_addr.to_string();

                // Obter destino UDP
                let (host, port) = {
                    let targets = udp_targets.lock().await;
                    if let Some(target) = targets.get(&client_addr_str) {
                        target.clone()
                    } else {
                        // Usar destino padrão
                        let parts: Vec<&str> = default_udp_host.split(':').collect();
                        let host = parts[0].to_string();
                        let port = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(7300);
                        (host, port)
                    }
                };

                println!("UDP Proxy: {} -> {}:{}", client_addr, host, port);

                // Enviar pacote UDP bruto ao destino
                let target_socket = UdpSocket::bind("[::]:0").await.unwrap();
                if let Err(e) = target_socket.send_to(data, format!("{}:{}", host, port)).await {
                    println!("Erro ao enviar UDP para {}:{}: {}", host, port, e);
                    continue;
                }

                // Receber resposta do destino
                let mut response_buffer = vec![0; 8192];
                if let Ok(Ok((bytes_received, _))) = timeout(Duration::from_secs(2), target_socket.recv_from(&mut response_buffer)).await {
                    // Encaminhar resposta ao cliente
                    if let Err(e) = socket.send_to(&response_buffer[..bytes_received], client_addr).await {
                        println!("Erro ao enviar resposta UDP para {}: {}", client_addr, e);
                    }
                }
            }
            Err(e) => {
                println!("Erro ao receber UDP: {}", e);
            }
        }
    }
}

async fn handle_client(client_stream: TcpStream, addr: std::net::SocketAddr, udp_targets: Arc<Mutex<HashMap<String, (String, u16)>>>) -> Result<(), Error> {
    let config = Config::from_args();
    // Adiciona timeout para manipulação completa do cliente
    let result = timeout(Duration::from_secs(30), async {
        client_stream
            .write_all(format!("HTTP/1.1 101 {}\r\n\r\n", config.status).as_bytes())
            .await?;

        let mut buffer = vec![0; 1024];
        let bytes_read = client_stream.read(&mut buffer).await?;
        let data_str = String::from_utf8_lossy(&buffer[..bytes_read]).to_string();

        // Extrair X-Real-Host para configurar destino UDP
        if let Some(host_port) = find_header(&data_str, "X-Real-Host") {
            let parts: Vec<&str> = host_port.split(':').collect();
            let host = parts[0].to_string();
            let port = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(7300); // Porta padrão 7300 para destino
            let mut targets = udp_targets.lock().await;
            targets.insert(addr.to_string(), (host, port));
        }

        client_stream
            .write_all(format!("HTTP/1.1 200 {}\r\n\r\n", config.status).as_bytes())
            .await?;

        let addr_proxy = match detect_protocol(&client_stream).await? {
            "ssh" => format!("0.0.0.0:{}", config.ssh_port),
            "openvpn" => format!("0.0.0.0:{}", config.openvpn_port),
            _ => format!("0.0.0.0:{}", config.ssh_port), // Padrão
        };

        let server_connect = TcpStream::connect(&addr_proxy).await;
        if server_connect.is_err() {
            println!("Erro ao iniciar conexão para o proxy {}", addr_proxy);
            return Ok(());
        }

        let server_stream = server_connect?;

        let (client_read, client_write) = client_stream.into_split();
        let (server_read, server_write) = server_stream.into_split();

        let client_read = Arc::new(Mutex::new(client_read));
        let client_write = Arc::new(Mutex::new(client_write));
        let server_read = Arc::new(Mutex::new(server_read));
        let server_write = Arc::new(Mutex::new(server_write));

        let client_to_server = transfer_data(client_read, server_write);
        let server_to_client = transfer_data(server_read, client_write);

        tokio::try_join!(client_to_server, server_to_client)?;
        Ok(())
    }).await;

    if let Err(e) = result {
        println!("Timeout na manipulação do cliente {}: {}", addr, e);
        Err(Error::new(ErrorKind::TimedOut, "Timeout na manipulação do cliente"))
    } else {
        result.unwrap()
    }
}

async fn transfer_data(
    read_stream: Arc<Mutex<tokio::net::tcp::OwnedReadHalf>>,
    write_stream: Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
) -> Result<(), Error> {
    let mut buffer = [0; 8192];
    loop {
        let bytes_read = {
            let mut read_guard = read_stream.lock().await;
            read_guard.read(&mut buffer).await?
        };

        if bytes_read == 0 {
            break;
        }

        let mut write_guard = write_stream.lock().await;
        write_guard.write_all(&buffer[..bytes_read]).await?;
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

async fn detect_protocol(stream: &TcpStream) -> Result<&'static str, Error> {
    let config = Config::from_args();
    let data = timeout(Duration::from_secs(config.timeout_secs), peek_stream(stream))
        .await
        .unwrap_or_else(|_| Ok(String::new()))?;
    if data.contains("SSH") {
        Ok("ssh")
    } else if data.contains("HTTP") {
        Ok("http")
    } else if data.is_empty() {
        Ok("ssh") // Padrão para SSH
    } else {
        Ok("openvpn")
    }
}

fn find_header(head: &str, header: &str) -> Option<String> {
    let header_line = format!("{}: ", header);
    let aux = head.find(&header_line)?;
    let start = aux + header_line.len();
    let end = head[start..].find("\r\n").map(|i| start + i)?;
    Some(head[start..end].to_string())
}

fn get_port() -> u16 {
    let args: Vec<String> = env::args().collect();
    let mut port = 80;

    for i in 1..args.len() {
        if args[i] == "--port" {
            if i + 1 < args.len() {
                port = args[i + 1].parse().unwrap_or(80);
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
