use std::env;
use std::io::{Error, ErrorKind};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::time::{Duration, timeout};
use deadpool::managed::{Pool, Manager, RecycleResult};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let port = get_port();
    let listener = TcpListener::bind(format!("[::]:{}", port)).await?;
    println!("Iniciando serviço na porta: {}", port);

    // Criando pools de conexões
    let ssh_pool = Pool::builder(TcpConnectionManager { addr: "0.0.0.0:22".parse().unwrap() })
        .max_size(10)
        .build()
        .unwrap();
    let ovpn_pool = Pool::builder(TcpConnectionManager { addr: "0.0.0.0:1194".parse().unwrap() })
        .max_size(10)
        .build()
        .unwrap();

    start_http(listener, ssh_pool, ovpn_pool).await;
    Ok(())
}

async fn start_http(listener: TcpListener, ssh_pool: Pool<TcpConnectionManager>, ovpn_pool: Pool<TcpConnectionManager>) {
    loop {
        match listener.accept().await {
            Ok((client_stream, addr)) => {
                let ssh_pool = ssh_pool.clone();
                let ovpn_pool = ovpn_pool.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(client_stream, ssh_pool, ovpn_pool).await {
                        println!("Erro ao processar cliente {}: {}", addr, e);
                    }
                });
            }
            Err(e) => println!("Erro ao aceitar conexão: {}", e),
        }
    }
}

async fn handle_client(
    mut client_stream: TcpStream,
    ssh_pool: Pool<TcpConnectionManager>,
    ovpn_pool: Pool<TcpConnectionManager>,
) -> Result<(), Error> {
    let status = get_status();
    client_stream
        .write_all(format!("HTTP/1.1 101 {}\r\n\r\n", status).as_bytes())
        .await?;

    let mut buffer = vec![0; 1024];
    client_stream.read(&mut buffer).await?;
    client_stream
        .write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes())
        .await?;

    // Detecção de protocolo
    let protocol = detect_protocol(&mut client_stream).await?;
    let pool = match protocol {
        Protocol::SSH | Protocol::Unknown => &ssh_pool,
        Protocol::OpenVPN | Protocol::HTTP => &ovpn_pool,
    };

    // Obtendo conexão do pool com retry
    let server_stream = connect_with_retry(pool, 3).await?;
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

// Melhorias Implementadas:

// 2. Detecção de Protocolo
#[derive(Debug)]
enum Protocol {
    SSH,
    OpenVPN,
    HTTP,
    Unknown,
}

async fn detect_protocol(stream: &mut TcpStream) -> Result<Protocol, Error> {
    let mut peek_buffer = vec![0; 8192];
    let bytes_peeked = timeout(Duration::from_secs(1), stream.peek(&mut peek_buffer)).await??;
    let data = &peek_buffer[..bytes_peeked];
    let data_str = String::from_utf8_lossy(data);

    if data_str.starts_with("SSH-") {
        Ok(Protocol::SSH)
    } else if data.len() >= 2 && data[0] == 0x00 && data[1] == 0x18 {
        Ok(Protocol::OpenVPN)
    } else if data_str.starts_with("GET") || data_str.starts_with("POST") {
        Ok(Protocol::HTTP)
    } else {
        Ok(Protocol::Unknown)
    }
}

// 3. Recuperação de Erros com Retry
async fn connect_with_retry(pool: &Pool<TcpConnectionManager>, retries: u32) -> Result<TcpStream, Error> {
    let mut attempt = 0;
    loop {
        match timeout(Duration::from_secs(5), pool.get()).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(e)) | Err(_) if attempt < retries => {
                println!("Falha na conexão (tentativa {}/{}): {:?}", attempt + 1, retries, e);
                tokio::time::sleep(Duration::from_secs(1)).await;
                attempt += 1;
            }
            Ok(Err(e)) | Err(_) => return Err(Error::new(ErrorKind::Other, format!("Falha após {} tentativas", retries))),
        }
    }
}

// 4. Pool de Conexões
struct TcpConnectionManager {
    addr: SocketAddr,
}

impl Manager for TcpConnectionManager {
    type Type = TcpStream;
    type Error = Error;

    async fn create(&self) -> Result<Self::Type, Self::Error> {
        TcpStream::connect(self.addr).await
    }

    async fn recycle(&self, conn: &mut Self::Type) -> RecycleResult<Self::Error> {
        conn.writable().await?;
        Ok(())
    }
}

// Funções auxiliares inalteradas
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
