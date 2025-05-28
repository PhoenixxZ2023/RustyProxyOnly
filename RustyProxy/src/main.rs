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
    timeout_secs: u64,
}

impl Config {
    fn from_args() -> Self {
        Config {
            port: get_port(),
            status: get_status(),
            ssh_port: 22,
            openvpn_port: 1194,
            timeout_secs: 1,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    // Iniciando o proxy com configurações
    let config = Config::from_args();
    let listener = TcpListener::bind(format!("[::]:{}", config.port)).await?;
    println!("Iniciando serviço na porta: {}", config.port);
    start_http(listener).await;
    Ok(())
}

async fn start_http(listener: TcpListener) {
    // Adiciona máximo de conexões simultâneas
    let max_connections = Arc::new(Semaphore::new(1000));

    loop {
        let permit = max_connections.clone().acquire_owned().await;
        match listener.accept().await {
            Ok((client_stream, addr)) => {
                tokio::spawn(async move {
                    let _permit = permit; // Mantém o permit ativo
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
    let config = Config::from_args();
    // Adiciona timeout para manipulação completa do cliente
    let result = timeout(Duration::from_secs(30), async {
        client_stream
            .write_all(format!("HTTP/1.1 101 {}\r\n\r\n", config.status).as_bytes())
            .await?;

        let mut buffer = vec![0; 1024];
        client_stream.read(&mut buffer).await?;
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
        println!("Timeout na manipulação do cliente: {}", e);
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
