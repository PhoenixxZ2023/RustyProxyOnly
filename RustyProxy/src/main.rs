use std::io::{Error, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;
use std::{env, thread};

const MAX_BUFFER_SIZE: usize = 8192;

fn main() {
    let listener = TcpListener::bind(format!("0.0.0.0:{}", get_port())).unwrap_or_else(|e| {
        eprintln!("Erro ao iniciar o listener: {}", e);
        std::process::exit(1);
    });
    println!("Proxy iniciado na porta {}", get_port());
    start_http(listener);
}

fn start_http(listener: TcpListener) {
    for stream in listener.incoming() {
        match stream {
            Ok(mut client_stream) => {
                thread::spawn(move || {
                    if let Err(e) = handle_client(&mut client_stream) {
                        eprintln!("Erro ao processar cliente: {}", e);
                    }
                });
            }
            Err(e) => {
                eprintln!("Erro ao aceitar conexão: {}", e);
            }
        }
    }
}

fn handle_client(client_stream: &mut TcpStream) -> Result<(), Error> {
    let status = get_status();
    let response = format!(
        "HTTP/1.1 101 {}\r\n\
        Date: {}\r\n\
        Content-Length: 0\r\n\
        Server: RustyProxy\r\n\r\n",
        status,
        httpdate::HttpDate::from(std::time::SystemTime::now())
    );

    client_stream.write_all(response.as_bytes())?;

    client_stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    client_stream.set_write_timeout(Some(Duration::from_secs(30)))?;

    let mut buffer: Vec<u8> = Vec::new();
    match peek_stream_with_buffer(client_stream, &mut buffer) {
        Ok(data_str) => {
            if is_websocket(&data_str) {
                client_stream.write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\n\
                    Upgrade: websocket\r\n\
                    Connection: Upgrade\r\n\
                    Date: \r\n\r\n",
                )?;
                return Ok(());
            }
        }
        Err(e) => return Err(e),
    }

    let addr_proxy = determine_proxy(client_stream)?;

    let server_stream = attempt_connection_with_backoff(&addr_proxy)?;

    let (mut client_read, mut client_write) = (client_stream.try_clone()?, client_stream.try_clone()?);
    let (mut server_read, mut server_write) = (server_stream.try_clone()?, server_stream);

    let client_to_server = thread::spawn(move || {
        transfer_data(&mut client_read, &mut server_write);
    });

    let server_to_client = thread::spawn(move || {
        transfer_data(&mut server_read, &mut client_write);
    });

    client_to_server.join().ok();
    server_to_client.join().ok();

    Ok(())
}

fn transfer_data(read_stream: &mut TcpStream, write_stream: &mut TcpStream) {
    let mut buffer = [0; MAX_BUFFER_SIZE];
    loop {
        match read_stream.read(&mut buffer) {
            Ok(0) => {
                eprintln!("Conexão encerrada pelo cliente.");
                break;
            }
            Ok(n) => {
                if n > MAX_BUFFER_SIZE {
                    eprintln!("Requisição excede o tamanho máximo permitido.");
                    break;
                }
                if let Err(e) = write_stream.write_all(&buffer[..n]) {
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

    safe_shutdown(read_stream);
    safe_shutdown(write_stream);
}

fn safe_shutdown(stream: &TcpStream) {
    match stream.take_error() {
        Ok(None) => {
            if let Err(e) = stream.shutdown(Shutdown::Both) {
                eprintln!("Erro ao encerrar conexão: {}", e);
            }
        }
        Ok(Some(e)) => eprintln!("Erro no socket antes de encerrar: {}", e),
        Err(e) => eprintln!("Erro ao verificar o socket: {}", e),
    }
}

fn peek_stream_with_buffer(read_stream: &mut TcpStream, buffer: &mut Vec<u8>) -> Result<String, Error> {
    let mut temp_buffer = vec![0; 4048];
    let bytes_read = read_stream.read(&mut temp_buffer)?;
    buffer.extend_from_slice(&temp_buffer[..bytes_read]);

    let data_str = String::from_utf8_lossy(buffer).to_string();
    Ok(data_str)
}

fn determine_proxy(client_stream: &mut TcpStream) -> Result<String, Error> {
    let mut buffer: Vec<u8> = Vec::new();
    let addr_proxy = if let Ok(data_str) = peek_stream_with_buffer(client_stream, &mut buffer) {
        if is_websocket(&data_str) {
            eprintln!("Conexão detectada como WebSocket.");
            get_http_proxy_address()
        } else if is_ssh(&data_str) {
            eprintln!("Conexão detectada como SSH.");
            get_ssh_address()
        } else if is_openvpn(&data_str) {
            eprintln!("Conexão detectada como OpenVPN.");
            get_openvpn_address()
        } else {
            eprintln!("Tráfego desconhecido, conectando ao proxy OpenVPN por padrão.");
            get_openvpn_address()
        }
    } else {
        eprintln!("Erro ao tentar ler dados do cliente. Conectando ao OpenVPN por padrão.");
        get_openvpn_address()
    };

    Ok(addr_proxy)
}

fn is_websocket(data: &str) -> bool {
    data.contains("Upgrade: websocket") && data.contains("Connection: Upgrade")
}

fn is_ssh(data: &str) -> bool {
    data.starts_with("SSH-")
}

fn is_openvpn(data: &str) -> bool {
    data.contains("OpenVPN") || data.contains("\x38\x10\x02\x00")
}

fn attempt_connection_with_backoff(addr_proxy: &str) -> Result<TcpStream, Error> {
    let mut retries = 0;
    let max_retries = 5;
    let mut delay = Duration::from_secs(1);

    loop {
        match TcpStream::connect(addr_proxy) {
            Ok(stream) => return Ok(stream),
            Err(e) if retries < max_retries => {
                eprintln!(
                    "Erro ao conectar ao proxy {}. Tentando novamente em {} segundos...",
                    addr_proxy,
                    delay.as_secs()
                );
                thread::sleep(delay);
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

fn get_ssh_address() -> String {
    env::var("SSH_PROXY_ADDR").unwrap_or_else(|_| String::from("0.0.0.0:22"))
}

fn get_openvpn_address() -> String {
    env::var("OPENVPN_PROXY_ADDR").unwrap_or_else(|_| String::from("0.0.0.0:1194"))
}

fn get_http_proxy_address() -> String {
    env::var("HTTP_PROXY_ADDR").unwrap_or_else(|_| String::from("0.0.0.0:8080"))
}
