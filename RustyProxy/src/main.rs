use tokio::io::{self, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use std::env;

const MAX_BUFFER_SIZE: usize = 8192;

#[tokio::main]
async fn main() -> io::Result<()> {
    let port = get_port();
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await.unwrap_or_else(|e| {
        eprintln!("Erro ao iniciar o listener: {}", e);
        std::process::exit(1);
    });

    println!("Proxy iniciado na porta {}", port);

    loop {
        match listener.accept().await {
            Ok((mut client_stream, _)) => {
                tokio::spawn(async move {
                    if let Err(e) = handle_client(&mut client_stream).await {
                        eprintln!("Erro ao processar cliente: {}", e);
                    }
                });
            }
            Err(e) => eprintln!("Erro ao aceitar conexão: {}", e),
        }
    }
}

async fn handle_client(client_stream: &mut TcpStream) -> io::Result<()> {
    let status = get_status();
    client_stream
        .write_all(format!("HTTP/1.1 101 {}\r\n\r\n", status).as_bytes())
        .await?;

    let data_str = peek_stream(client_stream).await.unwrap_or_default();
    if data_str.contains("HTTP") {
        let payload_str = data_str.to_lowercase();
        if payload_str.contains("websocket") || payload_str.contains("ws") {
            client_stream
                .write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes())
                .await?;
        }
    }

    let addr_proxy = determine_proxy(client_stream, &data_str).await?;

    let mut server_stream = attempt_connection_with_backoff(&addr_proxy).await?;

    let (mut client_read, mut client_write) = client_stream.split();
    let (mut server_read, mut server_write) = server_stream.split();

    let client_to_server = tokio::spawn(async move {
        transfer_data(&mut client_read, &mut server_write).await;
    });

    let server_to_client = tokio::spawn(async move {
        transfer_data(&mut server_read, &mut client_write).await;
    });

    client_to_server.await?;
    server_to_client.await?;

    Ok(())
}

async fn transfer_data<R, W>(read_stream: &mut R, write_stream: &mut W)
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let mut buffer = [0; MAX_BUFFER_SIZE];
    loop {
        match read_stream.read(&mut buffer).await {
            Ok(0) => {
                eprintln!("Conexão encerrada pelo cliente.");
                break;
            }
            Ok(n) => {
                if let Err(e) = write_stream.write_all(&buffer[..n]).await {
                    eprintln!("Erro de escrita: {}. Encerrando conexão.", e);
                    break;
                }
            }
            Err(e) => {
                eprintln!("Erro de leitura: {}. Encerrando conexão.", e);
                break;
            }
        }
    }
}

async fn peek_stream(stream: &mut TcpStream) -> io::Result<String> {
    let mut buffer = vec![0; 4096]; // Buffer maior para dados maiores
    let n = stream.peek(&mut buffer).await?;
    Ok(String::from_utf8_lossy(&buffer[..n]).to_string())
}

async fn determine_proxy(client_stream: &mut TcpStream, data_str: &str) -> io::Result<String> {
    if data_str.contains("SSH") {
        Ok(get_ssh_address())
    } else if data_str.contains("HTTP") && data_str.to_lowercase().contains("websocket") {
        eprintln!("Conexão WebSocket detectada.");
        Ok(get_openvpn_address())
    } else {
        eprintln!("Tráfego não identificado. Conectando ao OpenVPN por padrão.");
        Ok(get_openvpn_address())
    }
}

async fn attempt_connection_with_backoff(addr_proxy: &str) -> io::Result<TcpStream> {
    let mut retries = 0;
    let max_retries = 5;
    let mut delay = Duration::from_secs(1);

    loop {
        match TcpStream::connect(addr_proxy).await {
            Ok(stream) => return Ok(stream),
            Err(e) if retries < max_retries => {
                eprintln!(
                    "Erro ao conectar ao proxy {}. Tentando novamente em {} segundos...",
                    addr_proxy, delay.as_secs()
                );
                sleep(delay).await;
                retries += 1;
                delay *= 2;
            }
            Err(e) => {
                eprintln!(
                    "Falha ao conectar ao proxy {} após {} tentativas: {}",
                    addr_proxy, retries, e
                );
                return Err(e);
            }
        }
    }
}

fn get_port() -> u16 {
    let args: Vec<String> = env::args().collect();
    args.iter()
        .position(|arg| arg == "--port")
        .and_then(|pos| args.get(pos + 1))
        .and_then(|port| port.parse().ok())
        .unwrap_or(80)
}

fn get_status() -> String {
    let args: Vec<String> = env::args().collect();
    args.iter()
        .position(|arg| arg == "--status")
        .and_then(|pos| args.get(pos + 1))
        .cloned()
        .unwrap_or_else(|| String::from("@RustyManager"))
}

fn get_ssh_address() -> String {
    env::var("SSH_PROXY_ADDR").unwrap_or_else(|_| String::from("0.0.0.0:22"))
}

fn get_openvpn_address() -> String {
    env::var("OPENVPN_PROXY_ADDR").unwrap_or_else(|_| String::from("0.0.0.0:1194"))
}
