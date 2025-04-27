use std::env;
use std::io::Error;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};
use tracing::{error, info};
use tokio::signal;
use deadpool::managed::{self, Manager, Pool};

#[tokio::main]
async fn main() -> Result<(), Error> {
    // Configurando logging com tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    // Inicializando pools de conexões
    let (ssh_proxy_addr, openvpn_proxy_addr) = get_proxy_addresses();
    let ssh_pool = create_tcp_pool(ssh_proxy_addr.clone(), 10).await?;
    let openvpn_pool = create_tcp_pool(openvpn_proxy_addr.clone(), 10).await?;

    // Iniciando o proxy
    let port = get_port();
    let listener = TcpListener::bind(format!("[::]:{}", port)).await?;
    info!("Iniciando serviço na porta: {}", port);
    start_http(listener, ssh_pool, openvpn_pool).await;
    Ok(())
}

async fn start_http(listener: TcpListener, ssh_pool: TcpPool, openvpn_pool: TcpPool) {
    let mut sig = signal::ctrl_c();
    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((client_stream, addr)) => {
                        let ssh_pool = ssh_pool.clone();
                        let openvpn_pool = openvpn_pool.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_client(client_stream, ssh_pool, openvpn_pool).await {
                                error!("Erro ao processar cliente {}: {}", addr, e);
                            }
                        });
                    }
                    Err(e) => error!("Erro ao aceitar conexão: {}", e),
                }
            }
            _ = &mut sig => {
                info!("Desligando...");
                break;
            }
        }
    }
}

// Tipo para o pool de conexões TCP
type TcpPool = Pool<TcpConnectionManager>;

// Gerenciador de conexões TCP para o deadpool
struct TcpConnectionManager {
    addr: String,
}

impl Manager for TcpConnectionManager {
    type Type = TcpStream;
    type Error = Error;

    async fn create(&self) -> Result<TcpStream, Error> {
        TcpStream::connect(&self.addr).await
    }

    async fn recycle(&self, conn: &mut TcpStream) -> managed::RecycleResult<Error> {
        // Verifica se a conexão ainda é utilizável
        let mut buf = [0u8; 1];
        match conn.peek(&mut buf).await {
            Ok(_) => Ok(()),
            Err(e) => Err(managed::RecycleError::Backend(e)),
        }
    }
}

async fn create_tcp_pool(addr: String, max_size: usize) -> Result<TcpPool, Error> {
    let manager = TcpConnectionManager { addr };
    let pool = Pool::builder(manager)
        .max_size(max_size)
        .build()
        .map_err(|e| Error::new(std::io::ErrorKind::Other, e))?;
    Ok(pool)
}

async fn handle_client(mut client_stream: TcpStream, ssh_pool: TcpPool, openvpn_pool: TcpPool) -> Result<(), Error> {
    let status = get_status();
    client_stream
        .write_all(format!("HTTP/1.1 101 {}\r\n\r\n", status).as_bytes())
        .await?;

    let mut buffer = vec![0; 1024];
    client_stream.read(&mut buffer).await?;
    client_stream
        .write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes())
        .await?;

    let (ssh_proxy, openvpn_proxy) = get_proxy_addresses();
    let timeout_duration = get_timeout_duration();
    let result = timeout(timeout_duration, peek_stream(&mut client_stream)).await
        .unwrap_or_else(|_| Ok(String::new()));

    let (pool, addr_proxy) = if let Ok(data) = result {
        if is_ssh_protocol(&data) {
            (ssh_pool, ssh_proxy)
        } else {
            (openvpn_pool, openvpn_proxy)
        }
    } else {
        (ssh_pool, ssh_proxy)
    };

    let server_stream = pool.get().await
        .map_err(|e| {
            error!("Erro ao obter conexão do pool para {}: {}", addr_proxy, e);
            Error::new(std::io::ErrorKind::Other, e)
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

fn get_proxy_addresses() -> (String, String) {
    (
        env::var("SSH_PROXY").unwrap_or("0.0.0.0:22".to_string()), // SSH
        env::var("OPENVPN_PROXY").unwrap_or("0.0.0.0:1194".to_string()), // OpenVPN
    )
}

fn get_timeout_duration() -> Duration {
    let timeout_secs = env::var("PEEK_TIMEOUT")
        .unwrap_or("1".to_string())
        .parse::<u64>()
        .unwrap_or(1);
    Duration::from_secs(timeout_secs)
}

fn is_ssh_protocol(data: &str) -> bool {
    data.starts_with("SSH-") || data.is_empty()
}
