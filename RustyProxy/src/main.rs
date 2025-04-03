use std::env;
use std::io::Error;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

// Constantes para tamanhos de buffer
const BUFFER_SIZE: usize = 8192; // Tamanho do buffer para transferência de dados
const PEEK_BUFFER_SIZE: usize = 8192; // Tamanho do buffer para espiar o stream
const INITIAL_BUFFER_SIZE: usize = 1024; // Tamanho inicial do buffer do cliente

#[tokio::main]
async fn main() -> Result<(), Error> {
    // Iniciando o proxy
    let port = get_port();
    let listener = TcpListener::bind(format!("[::]:{}", port)).await?;
    println!("Iniciando serviço na porta: {}", port);
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
            Err(e) => {
                println!("Erro ao aceitar conexão: {}", e);
            }
        }
    }
}

async fn handle_client(mut client_stream: TcpStream) -> Result<(), Error> {
    let status = get_status();
    client_stream
        .write_all(format!("HTTP/1.1 101 {}\r\n\r\n", status).as_bytes())
        .await?;

    let mut buffer = vec![0; INITIAL_BUFFER_SIZE];
    client_stream.read(&mut buffer).await?;
    client_stream
        .write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes())
        .await?;

    let mut addr_proxy = "0.0.0.0:22"; // Padrão: SSH
    let result = timeout(Duration::from_secs(1), peek_stream(&mut client_stream))
        .await
        .unwrap_or_else(|_| Ok(String::new()));

    if let Ok(data) = result {
        if is_ssh_protocol(&data) {
            addr_proxy = "0.0.0.0:22"; // SSH
        } else if is_http_protocol(&data) {
            addr_proxy = "0.0.0.0:80"; // HTTP
        } else {
            addr_proxy = "0.0.0.0:1194"; // Outros (assumido como OpenVPN)
        }
    }

    // Propagação de erro com mensagem detalhada
    let server_stream = TcpStream::connect(addr_proxy)
        .await
        .map_err(|e| {
            println!("Erro ao iniciar conexão para o proxy {}: {}", addr_proxy, e);
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
    let mut buffer = [0; BUFFER_SIZE];
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
    let mut peek_buffer = vec![0; PEEK_BUFFER_SIZE];
    let bytes_peeked = stream.peek(&mut peek_buffer).await?;
    let data = &peek_buffer[..bytes_peeked];
    let data_str = String::from_utf8_lossy(data);
    Ok(data_str.to_string())
}

// Função para verificar se é um protocolo SSH
fn is_ssh_protocol(data: &str) -> bool {
    // SSH começa com "SSH-" seguido por versão (ex.: "SSH-2.0-OpenSSH_8.1")
    data.starts_with("SSH-") && data.len() > 4 && data.chars().nth(4).unwrap_or(' ') != ' '
}

// Função para verificar se é um protocolo HTTP
fn is_http_protocol(data: &str) -> bool {
    // Verifica métodos HTTP comuns no início da requisição
    data.starts_with("GET ") || 
    data.starts_with("POST ") || 
    data.starts_with("HEAD ") || 
    data.starts_with("PUT ") || 
    data.starts_with("DELETE ") || 
    data.starts_with("OPTIONS ") || 
    data.starts_with("PATCH ")
}

// Função para verificar se é um protocolo OpenVPN (heurística simples)
fn is_openvpn_protocol(data: &str) -> bool {
    // Mantida como heurística básica, mas agora menos relevante
    data.len() > 0 && !data.starts_with("SSH-") && data.chars().all(|c| c.is_ascii())
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
