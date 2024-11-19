use std::io::{Error, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;
use std::{env, thread};
use log::{error, info};
use simplelog::*;

const MAX_BUFFER_SIZE: usize = 8192;

fn main() {
    // Configuração do sistema de log
    TermLogger::init(
        LevelFilter::Info,
        ConfigBuilder::new()
            .set_time_to_local(true)
            .set_time_format_str("%Y-%m-%d %H:%M:%S")
            .build(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    )
    .unwrap();

    let listener = TcpListener::bind(format!("0.0.0.0:{}", get_port())).unwrap_or_else(|e| {
        error!("Erro ao iniciar o listener: {}", e);
        std::process::exit(1);
    });
    info!("Proxy iniciado na porta {}", get_port());
    start_http(listener);
}

fn start_http(listener: TcpListener) {
    for stream in listener.incoming() {
        match stream {
            Ok(mut client_stream) => {
                thread::spawn(move || {
                    if let Err(e) = handle_client(&mut client_stream) {
                        error!("Erro ao processar cliente: {}", e);
                    }
                });
            }
            Err(e) => {
                error!("Erro ao aceitar conexão: {}", e);
            }
        }
    }
}

fn handle_client(client_stream: &mut TcpStream) -> Result<(), Error> {
    let status = get_status();
    client_stream.write_all(format!("HTTP/1.1 101 {}\r\n\r\n", status).as_bytes())?;

    client_stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    client_stream.set_write_timeout(Some(Duration::from_secs(30)))?;

    match peek_stream(client_stream) {
        Ok(data_str) => {
            if data_str.contains("HTTP") {
                let _ = client_stream.read(&mut vec![0; 1024]);
                let payload_str = data_str.to_lowercase();
                if payload_str.contains("websocket") || payload_str.contains("ws") {
                    client_stream.write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes())?;
                }
            }
        }
        Err(e) => return Err(e),
    }

    let addr_proxy = determine_proxy(client_stream)?;

    let server_stream = attempt_connection_with_backoff(&addr_proxy)?;

    let (mut client_read, mut client_write) = (client_stream.try_clone()?, client_stream);
    let (mut server_read, mut server_write) = (server_stream.try_clone()?, server_stream);

    thread::spawn(move || transfer_data(&mut client_read, &mut server_write));
    thread::spawn(move || transfer_data(&mut server_read, &mut client_write));

    Ok(())
}

fn transfer_data(read_stream: &mut TcpStream, write_stream: &mut TcpStream) {
    let mut buffer = [0; MAX_BUFFER_SIZE];
    loop {
        match read_stream.read(&mut buffer) {
            Ok(0) => {
                info!(
                    "Conexão fechada com o cliente {}",
                    read_stream.peer_addr().unwrap_or_else(|_| "desconhecido".into())
                );
                break;
            }
            Ok(n) => {
                if n > MAX_BUFFER_SIZE {
                    error!("Requisição excede o tamanho máximo permitido.");
                    break;
                }
                if let Err(e) = write_stream.write_all(&buffer[..n]) {
                    error!(
                        "Erro de escrita para o cliente {}: {}. Tentando novamente...",
                        write_stream.peer_addr().unwrap_or_else(|_| "desconhecido".into()),
                        e
                    );
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
            }
            Err(e) => {
                error!(
                    "Erro de leitura do cliente {}: {}. Tentando novamente...",
                    read_stream.peer_addr().unwrap_or_else(|_| "desconhecido".into()),
                    e
                );
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        }
    }
}

fn peek_stream(read_stream: &TcpStream) -> Result<String, Error> {
    let mut peek_buffer = vec![0; 1024];
    let bytes_peeked = read_stream.peek(&mut peek_buffer)?;
    let data = &peek_buffer[..bytes_peeked];
    let data_str = String::from_utf8_lossy(data);
    Ok(data_str.to_string())
}

fn determine_proxy(client_stream: &mut TcpStream) -> Result<String, Error> {
    let addr_proxy = if let Ok(data_str) = peek_stream(client_stream) {
        if data_str.contains("SSH") {
            get_ssh_address()
        } else if data_str.contains("OpenVPN") {
            get_openvpn_address()
        } else {
            info!("Tipo de tráfego desconhecido, conectando ao proxy OpenVPN por padrão.");
            get_openvpn_address()
        }
    } else {
        info!("Erro ao tentar ler dados do cliente. Conectando ao OpenVPN por padrão.");
        get_openvpn_address()
    };
    Ok(addr_proxy)
}

fn attempt_connection_with_backoff(addr_proxy: &str) -> Result<TcpStream, Error> {
    let mut retries = 0;
    let max_retries = 5;
    let mut delay = Duration::from_secs(1);

    loop {
        match TcpStream::connect(addr_proxy) {
            Ok(stream) => return Ok(stream),
            Err(e) if retries < max_retries => {
                error!(
                    "Erro ao conectar ao proxy {}. Tentando novamente em {} segundos...",
                    addr_proxy, delay.as_secs()
                );
                thread::sleep(delay);
                retries += 1;
                delay *= 2;
            }
            Err(e) => {
                error!(
                    "Falha ao conectar ao proxy {} após {} tentativas: {}",
                    addr_proxy, retries, e
                );
                return Err(e);
            }
        }
    }
}

fn get_port() -> u16 {
    env::args()
        .nth(2)
        .and_then(|port_str| port_str.parse().ok())
        .unwrap_or(80)
}

fn get_status() -> String {
    env::args()
        .nth(3)
        .unwrap_or_else(|| String::from("@RustyManager"))
}

fn get_ssh_address() -> String {
    env::var("SSH_PROXY_ADDR").unwrap_or_else(|_| String::from("0.0.0.0:22"))
}

fn get_openvpn_address() -> String {
    env::var("OPENVPN_PROXY_ADDR").unwrap_or_else(|_| String::from("0.0.0.0:1194"))
}
