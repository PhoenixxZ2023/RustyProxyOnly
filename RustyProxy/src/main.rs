use std::env;
use std::io::Error;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::time::{Duration, timeout};

// Estrutura para centralizar configurações
struct Config {
    listen_port: u16,   // Porta onde o proxy escuta
    ssh_port: u16,      // Porta para encaminhar SSH
    openvpn_port: u16,  // Porta para encaminhar OpenVPN
    http_port: u16,     // Nova porta para HTTP
    status: String,     // Mensagem de status
}

impl Config {
    fn from_args() -> Self {
        let args: Vec<String> = env::args().collect();
        let mut config = Config {
            listen_port: 80,
            ssh_port: 22,
            openvpn_port: 1194,
            http_port: 8080,  // Porta padrão para HTTP
            status: String::from("@RustyManager"),
        };

        for i in 1..args.len() {
            match args[i].as_str() {
                "--port" => {
                    if i + 1 < args.len() {
                        config.listen_port = args[i + 1].parse().unwrap_or(80);
                    }
                }
                "--ssh-port" => {
                    if i + 1 < args.len() {
                        config.ssh_port = args[i + 1].parse().unwrap_or(22);
                    }
                }
                "--openvpn-port" => {
                    if i + 1 < args.len() {
                        config.openvpn_port = args[i + 1].parse().unwrap_or(1194);
                    }
                }
                "--http-port" => {  // Nova flag para porta HTTP
                    if i + 1 < args.len() {
                        config.http_port = args[i + 1].parse().unwrap_or(8080);
                    }
                }
                "--status" => {
                    if i + 1 < args.len() {
                        config.status = args[i + 1].clone();
                    }
                }
                _ => {}
            }
        }
        config
    }
}

// Enum atualizado com HTTP
enum Protocolo {
    SSH,
    OpenVPN,
    HTTP,         // Novo protocolo
    Desconhecido,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let config = Config::from_args();
    let listener = TcpListener::bind(format!("[::]:{}", config.listen_port)).await?;
    println!("Iniciando serviço na porta: {}", config.listen_port);
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
            Err(e) => println!("Erro ao aceitar conexão: {}", e),
        }
    }
}

async fn handle_client(mut client_stream: TcpStream) -> Result<(), Error> {
    let config = Config::from_args();

    // Envia resposta inicial HTTP 101
    client_stream
        .write_all(format!("HTTP/1.1 101 {}\r\n\r\n", config.status).as_bytes())
        .await?;

    let mut buffer = vec![0; 1024];
    client_stream.read(&mut buffer).await?;
    client_stream
        .write_all(format!("HTTP/1.1 200 {}\r\n\r\n", config.status).as_bytes())
        .await?;

    // Detecta o protocolo com timeout
    let addr_proxy = match timeout(Duration::from_secs(1), detectar_protocolo(&client_stream)).await {
        Ok(Protocolo::SSH) => format!("0.0.0.0:{}", config.ssh_port),
        Ok(Protocolo::OpenVPN) => format!("0.0.0.0:{}", config.openvpn_port),
        Ok(Protocolo::HTTP) => format!("0.0.0.0:{}", config.http_port),  // Encaminha para HTTP
        Ok(Protocolo::Desconhecido) | Err(_) => {
            println!("Protocolo desconhecido ou timeout, usando SSH como fallback");
            format!("0.0.0.0:{}", config.ssh_port)
        }
    };

    let server_stream = TcpStream::connect(&addr_proxy).await
        .map_err(|e| {
            println!("Erro ao conectar ao {} na porta {}: {}", 
                if addr_proxy.contains(&config.ssh_port.to_string()) { "SSH" }
                else if addr_proxy.contains(&config.openvpn_port.to_string()) { "OpenVPN" }
                else { "HTTP" }, 
                addr_proxy, 
                e);
            e
        })?;

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
}

async fn transfer_data(
    read_stream: Arc<Mutex<tokio::net::tcp::OwnedReadHalf>>,
    write_stream: Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
) -> Result<(), Error> {
    let mut buffer = [0; 8192];
    loop {
        let bytes_read = read_stream.lock().await.read(&mut buffer).await?;
        if bytes_read == 0 { break; }
        write_stream.lock().await.write_all(&buffer[..bytes_read]).await?;
    }
    Ok(())
}

// Função atualizada para detectar HTTP
async fn detectar_protocolo(stream: &TcpStream) -> Protocolo {
    match peek_stream(stream).await {
        Ok(data) => {
            if data.starts_with("SSH-") {
                Protocolo::SSH
            } else if data.starts_with("GET ") || data.starts_with("POST ") || data.starts_with("HEAD ") {
                Protocolo::HTTP  // Identifica métodos HTTP comuns
            } else if !data.is_empty() {
                Protocolo::OpenVPN  // Assume OpenVPN se não for SSH ou HTTP
            } else {
                Protocolo::Desconhecido
            }
        }
        Err(e) => {
            println!("Erro ao detectar protocolo: {}", e);
            Protocolo::Desconhecido
        }
    }
}

async fn peek_stream(stream: &TcpStream) -> Result<String, Error> {
    let mut peek_buffer = vec![0; 8192];
    let bytes_peeked = stream.peek(&mut peek_buffer).await?;
    let data = &peek_buffer[..bytes_peeked];
    let data_str = String::from_utf8_lossy(data);
    Ok(data_str.to_string())
}
