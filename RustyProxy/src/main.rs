use std::env;
use std::io::Error;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};
use std::collections::HashMap;
use tokio::signal;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let port = get_port();
    let listener = TcpListener::bind(format!("[::]:{}", port)).await?;
    println!("Servidor iniciado na porta: {}", port);

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        start_proxy(listener, shutdown_rx).await;
    });

    signal::ctrl_c().await?;
    println!("Recebido sinal de encerramento, finalizando...");
    if shutdown_tx.send(()).is_err() {
        eprintln!("Aviso: O canal de encerramento já foi fechado.");
    }

    tokio::time::sleep(Duration::from_secs(5)).await;
    Ok(())
}

async fn start_proxy(listener: TcpListener, mut shutdown_rx: tokio::sync::oneshot::Receiver<()>) {
    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((client_stream, addr)) => {
                        println!("Nova conexão de: {}", addr);
                        tokio::spawn(async move {
                            if let Err(e) = handle_client(client_stream).await {
                                eprintln!("Erro ao processar cliente {}: {}", addr, e);
                            }
                        });
                    }
                    Err(e) => eprintln!("Erro ao aceitar conexão: {}", e),
                }
            }
            _ = &mut shutdown_rx => {
                println!("Encerrando proxy...");
                break;
            }
        }
    }
}

struct ProxyConfig {
    protocol_map: HashMap<String, String>,
}

impl ProxyConfig {
    fn new() -> Self {
        let mut map = HashMap::new();
        map.insert("SSH".to_string(), "0.0.0.0:22".to_string());
        map.insert("HTTP".to_string(), "0.0.0.0:80".to_string());
        map.insert("OpenVPN".to_string(), "0.0.0.0:1194".to_string());
        Self { protocol_map: map }
    }

    fn get_destination(&self, data: &str) -> &str {
        if data.starts_with("SSH") || data.contains("ssh") {
            self.protocol_map.get("SSH").unwrap()
        } else if data.starts_with("GET") || data.starts_with("POST") || data.contains("HTTP") {
            self.protocol_map.get("HTTP").unwrap()
        } else if data.contains("OpenVPN") {
            self.protocol_map.get("OpenVPN").unwrap()
        } else {
            self.protocol_map.get("SSH").unwrap()
        }
    }
}

async fn handle_client(mut client_stream: TcpStream) -> Result<(), Error> {
    let config = ProxyConfig::new();
    let status = get_status();
    client_stream
        .write_all(format!("HTTP/1.1 101 {}\r\n\r\n", status).as_bytes())
        .await?;

    let mut buffer = [0; 1024];
    client_stream.read(&mut buffer).await?;
    client_stream
        .write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes())
        .await?;

    let peeked_data = peek_stream(&client_stream, 2).await.unwrap_or_default();
    let addr_proxy = config.get_destination(&peeked_data);

    let server_stream = match TcpStream::connect(addr_proxy).await {
        Ok(stream) => stream,
        Err(_) => {
            eprintln!("Erro ao conectar-se ao servidor proxy em {}", addr_proxy);
            return Ok(());
        }
    };

    let (client_read, client_write) = client_stream.into_split();
    let (server_read, server_write) = server_stream.into_split();

    let client_read = Arc::new(Mutex::new(client_read));
    let client_write = Arc::new(Mutex::new(client_write));
    let server_read = Arc::new(Mutex::new(server_read));
    let server_write = Arc::new(Mutex::new(server_write));

    tokio::try_join!(
        transfer_data(client_read.clone(), server_write.clone()),
        transfer_data(server_read.clone(), client_write.clone())
    )?;

    Ok(())
}

async fn transfer_data(
    read_stream: Arc<Mutex<tokio::net::tcp::OwnedReadHalf>>,
    write_stream: Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
) -> Result<(), Error> {
    let mut buffer = Vec::with_capacity(8192);
    loop {
        buffer.clear();
        let bytes_read = {
            let mut reader = read_stream.lock().await;
            reader.read_buf(&mut buffer).await?
        };

        if bytes_read == 0 {
            break;
        }

        let mut writer = write_stream.lock().await;
        writer.write_all(&buffer[..bytes_read]).await?;
    }
    Ok(())
}

async fn peek_stream(stream: &TcpStream, timeout_secs: u64) -> Result<String, Error> {
    let mut buffer = vec![0; 8192];
    match timeout(Duration::from_secs(timeout_secs), stream.peek(&mut buffer)).await {
        Ok(Ok(bytes_peeked)) => Ok(String::from_utf8_lossy(&buffer[..bytes_peeked]).to_string()),
        Ok(Err(e)) => Err(e),
        Err(_) => {
            eprintln!("Tempo limite atingido para leitura inicial.");
            Ok("".to_string())
        }
    }
}

fn get_port() -> u16 {
    env::args()
        .nth(1)
        .unwrap_or_else(|| "80".to_string())
        .parse()
        .unwrap_or(80)
}

fn get_status() -> String {
    env::args()
        .nth(2)
        .unwrap_or_else(|| "@RustyManager".to_string())
}
