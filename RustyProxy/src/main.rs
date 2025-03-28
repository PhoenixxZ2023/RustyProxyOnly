use std::env;
use std::io::Error;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

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
                    if let Err(e) = handle_client(client_stream, addr).await {
                        eprintln!("Erro ao processar cliente {}: {}", addr, e);
                    }
                });
            }
            Err(e) => eprintln!("Erro ao aceitar conexão: {}", e),
        }
    }
}

async fn handle_client(mut client_stream: TcpStream, addr: SocketAddr) -> Result<(), Error> {
    let mut buffer = [0; 4096];
    let bytes_read = client_stream.read(&mut buffer).await?;
    let request = String::from_utf8_lossy(&buffer[..bytes_read]).to_string();

    // Parsear a requisição manualmente
    let (method, host, port, keep_alive) = match parse_http_request(&request) {
        Ok((m, h, p, k)) => (m, h, p, k),
        Err(_) => {
            client_stream
                .write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n")
                .await?;
            return Ok(());
        }
    };

    let dest_addr = match (method.as_str(), &host, port) {
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

    let mut server_stream = match TcpStream::connect(&dest_addr).await {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!(
                "Erro ao processar cliente {}: Falha ao conectar a {}: {}",
                addr, dest_addr, e
            );
            client_stream
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                .await?;
            return Ok(());
        }
    };

    match method.as_str() {
        "CONNECT" => {
            client_stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
            server_stream
                .write_all(&buffer[..bytes_read])
                .await?;

            let (tx_client_to_server, mut rx_client_to_server) = mpsc::channel::<Vec<u8>>(100);
            let (tx_server_to_client, mut rx_server_to_client) = mpsc::channel::<Vec<u8>>(100);

            // Leitura do cliente para o servidor
            tokio::spawn(async move {
                let mut buffer = [0; 8192];
                loop {
                    match client_stream.read(&mut buffer).await {
                        Ok(0) => break,
                        Ok(n) => {
                            let data = buffer[..n].to_vec();
                            if tx_client_to_server.send(data).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });

            // Leitura do servidor para o cliente
            tokio::spawn(async move {
                let mut buffer = [0; 8192];
                loop {
                    match server_stream.read(&mut buffer).await {
                        Ok(0) => break,
                        Ok(n) => {
                            let data = buffer[..n].to_vec();
                            if tx_server_to_client.send(data).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });

            // Escrita do cliente para o servidor
            tokio::spawn(async move {
                while let Some(data) = rx_client_to_server.recv().await {
                    if server_stream.write_all(&data).await.is_err() {
                        break;
                    }
                }
            });

            // Escrita do servidor para o cliente
            while let Some(data) = rx_server_to_client.recv().await {
                if client_stream.write_all(&data).await.is_err() {
                    break;
                }
            }
        }
        _ => {
            server_stream
                .write_all(&buffer[..bytes_read])
                .await?;
            let mut keep_alive = keep_alive;

            loop {
                let mut response_buffer = [0; 8192];
                let bytes_read = server_stream.read(&mut response_buffer).await?;
                if bytes_read == 0 {
                    break;
                }
                client_stream
                    .write_all(&response_buffer[..bytes_read])
                    .await?;

                if !keep_alive {
                    break;
                }

                // Ler próxima requisição se keep-alive
                let bytes_read = client_stream.read(&mut buffer).await?;
                if bytes_read == 0 {
                    break;
                }
                let next_request = String::from_utf8_lossy(&buffer[..bytes_read]).to_string();
                let (next_method, next_host, next_port, next_keep_alive) = match parse_http_request(&next_request) {
                    Ok((m, h, p, k)) => (m, h, p, k),
                    Err(_) => break,
                };

                let next_dest_addr = format!(
                    "{}:{}",
                    next_host.as_ref().unwrap_or(&"".to_string()),
                    next_port.unwrap_or(80)
                );
                if next_dest_addr != dest_addr {
                    server_stream = TcpStream::connect(&next_dest_addr).await?;
                }
                server_stream
                    .write_all(&buffer[..bytes_read])
                    .await?;
                keep_alive = next_keep_alive;
            }
        }
    }

    Ok(())
}

fn parse_http_request(request: &str) -> Result<(String, Option<String>, Option<u16>, bool), ()> {
    let lines: Vec<&str> = request.lines().collect();
    if lines.is_empty() {
        return Err(());
    }

    // Parsear a primeira linha (método e URI)
    let first_line_parts: Vec<&str> = lines[0].split_whitespace().collect();
    if first_line_parts.len() < 2 {
        return Err(());
    }
    let method = first_line_parts[0].to_string();

    let mut host = None;
    let mut port = None;
    let mut keep_alive = false;

    // Procurar cabeçalho Host e Connection
    for line in lines.iter().skip(1) {
        if line.to_lowercase().starts_with("host:") {
            let host_value = line[5..].trim();
            let parts: Vec<&str> = host_value.split(':').collect();
            host = Some(parts[0].to_string());
            if parts.len() > 1 {
                port = parts[1].parse().ok();
            }
        } else if line.to_lowercase().starts_with("connection:") {
            if line[10..].trim().to_lowercase().contains("keep-alive") {
                keep_alive = true;
            }
        }
    }

    // Para CONNECT, o destino está na primeira linha
    if method == "CONNECT" {
        let target = first_line_parts[1];
        let parts: Vec<&str> = target.split(':').collect();
        host = Some(parts[0].to_string());
        if parts.len() > 1 {
            port = parts[1].parse().ok();
        }
    }

    Ok((method, host, port, keep_alive))
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
