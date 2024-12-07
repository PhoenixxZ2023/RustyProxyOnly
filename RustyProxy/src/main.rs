use std::env;
use std::io::{Error, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

const MAX_BUFFER_SIZE: usize = 8192;

fn main() {
    let port = get_port();
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).unwrap_or_else(|e| {
        eprintln!("Erro ao iniciar o listener: {}", e);
        std::process::exit(1);
    });

    println!("Proxy iniciado na porta {}", port);

    for stream in listener.incoming() {
        match stream {
            Ok(mut client_stream) => {
                thread::spawn(move || {
                    if let Err(e) = handle_client(&mut client_stream) {
                        eprintln!("Erro ao processar cliente: {}", e);
                    }
                });
            }
            Err(e) => eprintln!("Erro ao aceitar conexão: {}", e),
        }
    }
}

fn handle_client(client_stream: &mut TcpStream) -> Result<(), Error> {
    let status = get_status();
    client_stream.write_all(format!("HTTP/1.1 101 {}\r\n\r\n", status).as_bytes())?;

    client_stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    client_stream.set_write_timeout(Some(Duration::from_secs(30)))?;

    let data_str = peek_stream(client_stream).unwrap_or_default();

    // Detecta WebSocket e outros tipos de tráfego HTTP
    if data_str.contains("HTTP") {
        if let Some(proxy_addr) = detect_websocket(&data_str) {
            client_stream.write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes())?;
            return Ok(client_stream.connect(proxy_addr)?);
        }
    }

    // Detecta SSH ou OpenVPN
    let addr_proxy = determine_proxy(client_stream, &data_str)?;

    let mut server_stream = attempt_connection_with_backoff(&addr_proxy)?;

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

    // Fechar streams ao final
    read_stream.shutdown(Shutdown::Read).ok();
    write_stream.shutdown(Shutdown::Write).ok();
}

fn peek_stream(stream: &TcpStream) -> Result<String, Error> {
    let mut peek_buffer = vec![0; 4096];
    let bytes_peeked = stream.peek(&mut peek_buffer)?;
    Ok(String::from_utf8_lossy(&peek_buffer[..bytes_peeked]).to_string())
}

fn detect_websocket(data_str: &str) -> Option<String> {
    // Verifica cabeçalho "Upgrade: websocket"
    if data_str.to_lowercase().contains("upgrade: websocket") {
        eprintln!("WebSocket Detectado!");
        return Some(get_openvpn_address());
    }
    None
}

fn determine_proxy(client_stream: &mut TcpStream, data_str: &str) -> Result<String, Error> {
    if data_str.starts_with("SSH") {
        eprintln!("Conexão SSH Detectada!");
        Ok(get_ssh_address())
    } else if data_str.contains("HTTP") {
        if let Some(proxy_addr) = detect_websocket(data_str) {
            return Ok(proxy_addr);
        }
        eprintln!("Requisição HTTP detectada.");
        Ok(get_openvpn_address())
    } else {
        eprintln!("Tráfego não identificado. Conectando ao OpenVPN por padrão.");
        Ok(get_openvpn_address())
    }
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
                    addr_proxy, delay.as_secs()
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
