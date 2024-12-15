use tokio::io::{self, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, Duration};
use std::env;

const MAX_BUFFER_SIZE: usize = 8192;

#[tokio::main]
async fn main() {
    let port = get_port();
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await.unwrap_or_else(|e| {
        eprintln!("Erro ao iniciar o listener: {}", e);
        std::process::exit(1);
    });

    println!("Proxy iniciado na porta {}", port);

    while let Ok((mut client_stream, _)) = listener.accept().await {
        tokio::spawn(async move {
            if let Err(e) = handle_client(&mut client_stream).await {
                eprintln!("Erro ao processar cliente: {}", e);
            }
        });
    }
}

async fn handle_client(client_stream: &mut TcpStream) -> io::Result<()> {
    let status = get_status();

    client_stream.write_all(format!("HTTP/1.1 101 {}\r\n\r\n", status).as_bytes()).await?;

    match peek_stream(client_stream).await {
        Ok(data_str) => {
            if data_str.contains("HTTP") {
                let _ = client_stream.read(&mut vec![0; 1024]).await;
                let payload_str = data_str.to_lowercase();
                if payload_str.contains("websocket") || payload_str.contains("ws") {
                    client_stream.write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes()).await?;
                }
            }
        }
        Err(e) => return Err(e),
    }

    let addr_proxy = determine_proxy(client_stream).await?;

    let server_stream = attempt_connection_with_backoff(&addr_proxy).await?;

    let (mut client_read, mut client_write) = client_stream.split();
    let (mut server_read, mut server_write) = server_stream.into_split();

    let client_to_server = tokio::spawn(async move {
        transfer_data(&mut client_read, &mut server_write).await;
    });

    let server_to_client = tokio::spawn(async move {
        transfer_data(&mut server_read, &mut client_write).await;
    });

    tokio::try_join!(client_to_server, server_to_client).ok();

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
                if n > MAX_BUFFER_SIZE {
                    eprintln!("Requisição excede o tamanho máximo permitido.");
                    break;
                }
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

async fn peek_stream(stream: &TcpStream) -> io::Result<String> {
    let mut peek_buffer = vec![0; 1024];
    let bytes_peeked = stream.peek(&mut peek_buffer)?;
    let data = &peek_buffer[..bytes_peeked];
    let data_str = String::from_utf8_lossy(data);
    Ok(data_str.to_string())
}

async fn determine_proxy(client_stream: &mut TcpStream) -> io::Result<String> {
    if let Ok(data_str) = peek_stream(client_stream).await {
        if data_str.contains("SSH") {
            Ok(get_ssh_address())
        } else if data_str.contains("OpenVPN") {
            Ok(get_openvpn_address())
        } else {
            eprintln!("Tipo de tráfego desconhecido, conectando ao proxy OpenVPN por padrão.");
            Ok(get_openvpn_address())
        }
    } else {
        eprintln!("Erro ao tentar ler dados do cliente. Conectando ao OpenVPN por padrão.");
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
                    addr_proxy,
                    delay.as_secs()
                );
                sleep(delay).await;
                retries += 1;
                delay *= 2;
            }
            Err(e) => {
                eprintln!("Falha ao conectar ao proxy {} após {} tentativas: {}", addr_proxy, retries, e);
                return Err(e);
            }
        }
    }
}

fn get_port() -> u16 {
    env::args()
        .skip_while(|arg| arg != "--port")
        .nth(1)
        .and_then(|port_str| port_str.parse().ok())
        .unwrap_or(80)
}

fn get_status() -> String {
    env::args()
        .skip_while(|arg| arg != "--status")
        .nth(1)
        .unwrap_or_else(|| String::from("@RustyManager"))
}

fn get_ssh_address() -> String {
    env::var("SSH_PROXY_ADDR").unwrap_or_else(|_| String::from("0.0.0.0:22"))
}

fn get_openvpn_address() -> String {
    env::var("OPENVPN_PROXY_ADDR").unwrap_or_else(|_| String::from("0.0.0.0:1194"))
}
