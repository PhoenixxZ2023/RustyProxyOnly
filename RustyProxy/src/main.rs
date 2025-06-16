use std::env;
use std::io::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{timeout, Duration};
const BUFFER_SIZE: usize = 32768;
const SSH_KEYWORD: &str = "SSH";
const SSH_TARGET_ADDR: &str = "127.0.0.1:22";       
const OPENVPN_TARGET_ADDR: &str = "127.0.0.1:1194"; 

#[tokio::main]
async fn main() -> Result<(), Error> {
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

    let mut buffer = vec![0; BUFFER_SIZE];
    client_stream.read(&mut buffer).await?;
    client_stream
        .write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes())
        .await?;

    let peek_result = timeout(Duration::from_secs(1), peek_stream(&mut client_stream)).await;

    let data_to_check = match peek_result {
        Ok(Ok(data)) => data,
        Ok(Err(e)) => {
            println!("Erro ao espiar o stream: {}", e);
            String::new()
        }
        Err(_) => {
            println!("Tempo limite excedido ao espiar o stream.");
            String::new()
        }
    };

    let addr_proxy: &str;
    if data_to_check.contains(SSH_KEYWORD) || data_to_check.is_empty() {
        addr_proxy = SSH_TARGET_ADDR;
    } else {
        addr_proxy = OPENVPN_TARGET_ADDR;
    }

    let server_connect = TcpStream::connect(addr_proxy).await;
    let server_stream = match server_connect {
        Ok(s) => s,
        Err(e) => {
            println!(
                "Erro ao iniciar conexão para o proxy {}: {}",
                addr_proxy, e
            );
            return Ok(());
        }
    };

    let (client_read, client_write) = client_stream.into_split();
    let (server_read, server_write) = server_stream.into_split();

    let client_to_server = transfer_data(client_read, server_write);
    let server_to_client = transfer_data(server_read, client_write);

    tokio::try_join!(client_to_server, server_to_client)?;

    Ok(())
}

async fn transfer_data(
    mut read_stream: tokio::net::tcp::OwnedReadHalf,
    mut write_stream: tokio::net::tcp::OwnedWriteHalf,
) -> Result<(), Error> {
    let mut buffer = [0; BUFFER_SIZE];
    loop {
        let bytes_read = read_stream.read(&mut buffer).await?;

        if bytes_read == 0 {
            break;
        }

        write_stream.write_all(&buffer[..bytes_read]).await?;
    }

    Ok(())
}

async fn peek_stream(stream: &TcpStream) -> Result<String, Error> {
    let mut peek_buffer = vec![0; 2048];
    let bytes_peeked = stream.peek(&mut peek_buffer).await?;
    let data = &peek_buffer[..bytes_peeked];
    let data_str = String::from_utf8_lossy(data);
    Ok(data_str.to_string())
}

fn get_arg_value(arg_name: &str, default_value: &str) -> String {
    let args: Vec<String> = env::args().collect();
    for i in 1..args.len() {
        if args[i] == arg_name {
            if i + 1 < args.len() {
                return args[i + 1].clone();
            }
        }
    }
    default_value.to_string()
}

fn get_port() -> u16 {
    get_arg_value("--port", "80").parse().unwrap_or(80)
}

fn get_status() -> String {
    get_arg_value("--status", "@RustyManager")
}
