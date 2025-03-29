use std::env;
use std::io::Error;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let port = get_port();
    let listener = TcpListener::bind(format!("[::]:{}", port)).await?;
    println!("Servidor iniciado na porta: {}", port);
    start_proxy(listener).await;
    Ok(())
}

async fn start_proxy(listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((client_stream, addr)) => {
                println!("Nova conexão de: {}", addr);
                tokio::spawn(async move {
                    if let Err(e) = timeout(Duration::from_secs(30), handle_client(client_stream, addr)).await {
                        eprintln!("Erro ao processar cliente {}: Timeout ou erro: {:?}", addr, e);
                    }
                });
            }
            Err(e) => eprintln!("Erro ao aceitar conexão: {}", e),
        }
    }
}

async fn handle_client(mut client_stream: TcpStream, addr: SocketAddr) -> Result<(), Error> {
    let is_simple_mode = env::args().any(|arg| arg == "--simple"); // Verifica se modo simples está ativado
    let status = get_status();

    if is_simple_mode {
        // Modo simplificado: apenas responde 101 e 200
        timeout(Duration::from_secs(5), 
            client_stream.write_all(format!("HTTP/1.1 101 {}\r\n\r\n", status).as_bytes())
        ).await??;

        let mut buffer = [0; 1024];
        match timeout(Duration::from_secs(5), client_stream.read(&mut buffer)).await {
            Ok(Ok(bytes_read)) => println!("Lidos {} bytes do cliente {}", bytes_read, addr),
            Ok(Err(e)) => return Err(e),
            Err(_) => eprintln!("Timeout ao ler dados do cliente {}", addr),
        };

        timeout(Duration::from_secs(5), 
            client_stream.write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes())
        ).await??;
    } else {
        // Modo proxy completo
        let mut buffer = [0; 4096];
        let bytes_read = timeout(Duration::from_secs(10), client_stream.read(&mut buffer)).await??;
        let request = String::from_utf8_lossy(&buffer[..bytes_read]).to_string();

        let (method, host, port) = parse_http_request(&request);

        let dest_addr = match (method.as_str(), host, port) {
            ("CONNECT", Some(host), Some(port)) => format!("{}:{}", host, port),
            ("CONNECT", Some(host), None) => format!("{}:443", host),
            (_, Some(host), Some(port)) => format!("{}:{}", host, port),
            (_, Some(host), None) => format!("{}:80", host),
            _ => {
                client_stream
                    .write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n")
                    .await?;
                return Ok(());
            }
        };

        let mut server_stream = match timeout(Duration::from_secs(15), TcpStream::connect(&dest_addr)).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => {
                eprintln!(
                    "Erro ao processar cliente {}: Falha ao conectar a {}: {}",
                    addr, dest_addr, e
                );
                client_stream
                    .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                    .await?;
                return Ok(());
            }
            Err(_) => {
                eprintln!(
                    "Erro ao processar cliente {}: Timeout ao conectar a {}",
                    addr, dest_addr
                );
                client_stream
                    .write_all(b"HTTP/1.1 504 Gateway Timeout\r\n\r\n")
                    .await?;
                return Ok(());
            }
        };

        match method.as_str() {
            "CONNECT" => {
                client_stream
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await?;

                server_stream.write_all(&buffer[..bytes_read]).await?;

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
            }
            _ => {
                server_stream.write_all(&buffer[..bytes_read]).await?;

                let mut response_buffer = [0; 8192];
                loop {
                    let bytes_read = match timeout(Duration::from_secs(10), server_stream.read(&mut response_buffer)).await {
                        Ok(Ok(0)) => break,
                        Ok(Ok(n)) => n,
                        Ok(Err(e)) => return Err(e),
                        Err(_) => {
                            eprintln!("Timeout ao ler resposta do servidor para cliente {}", addr);
                            break;
                        }
                    };
                    client_stream
                        .write_all(&response_buffer[..bytes_read])
                        .await?;
                }
            }
        }
    }

    Ok(())
}

async fn transfer_data(
    read_stream: Arc<Mutex<tokio::net::tcp::OwnedReadHalf>>,
    write_stream: Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
) -> Result<(), Error> {
    let mut buffer = [0; 8192];
    loop {
        let bytes_read = {
            let mut reader = read_stream.lock().await;
            match timeout(Duration::from_secs(10), reader.read(&mut buffer)).await {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => n,
                Ok(Err(_)) => break,
                Err(_) => {
                    eprintln!("Timeout na transferência de dados");
                    break;
                }
            }
        };

        let mut writer = write_stream.lock().await;
        if writer.write_all(&buffer[..bytes_read]).await.is_err() {
            break;
        }
    }
    Ok(())
}

fn parse_http_request(request: &str) -> (String, Option<String>, Option<u16>) {
    let lines: Vec<&str> = request.lines().collect();
    if lines.is_empty() {
        return (String::new(), None, None);
    }

    let first_line_parts: Vec<&str> = lines[0].split_whitespace().collect();
    let method = first_line_parts.get(0).unwrap_or(&"").to_string();

    let mut host = None;
    let mut port = None;

    for line in lines.iter().skip(1) {
        if line.to_lowercase().starts_with("host:") {
            let host_value = line[5..].trim();
            let parts: Vec<&str> = host_value.split(':').collect();
            host = Some(parts[0].to_string());
            if parts.len() > 1 {
                port = parts[1].parse().ok();
            }
            break;
        }
    }

    if method == "CONNECT" {
        if let Some(target) = first_line_parts.get(1) {
            let parts: Vec<&str> = target.split(':').collect();
            host = Some(parts[0].to_string());
            if parts.len() > 1 {
                port = parts[1].parse().ok();
            }
        }
    }

    (method, host, port)
}

fn get_port() -> u16 {
    env::args()
        .nth(2)
        .unwrap_or_else(|| "80".to_string())
        .parse()
        .unwrap_or(80)
}

fn get_status() -> String {
    env::args()
        .nth(4)
        .unwrap_or_else(|| "@RustyManager".to_string())
}
