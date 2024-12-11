use log::{error, info, warn};
use std::io::{Error, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;
use std::{env, thread};

const MAX_BUFFER_SIZE: usize = 8192;

fn main() {
    env_logger::init(); // Initialize the logger

    let port = get_port();
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).unwrap_or_else(|e| {
        error!("Failed to start listener: {}", e);
        std::process::exit(1);
    });

    info!("Proxy started on port {}", port);
    start_http(listener);
}

fn start_http(listener: TcpListener) {
    for stream in listener.incoming() {
        match stream {
            Ok(mut client_stream) => {
                thread::spawn(move || {
                    if let Err(e) = handle_client(&mut client_stream) {
                        error!("Error processing client: {}", e);
                    }
                });
            }
            Err(e) => {
                warn!("Error accepting connection: {}", e);
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
            if is_websocket(&data_str) {
                client_stream.write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
                )?;
                return Ok(());
            }
        }
        Err(e) => {
            warn!("Failed to peek stream: {}", e);
            return Err(e);
        }
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
                info!("Connection closed by client.");
                break;
            }
            Ok(n) => {
                if let Err(e) = write_stream.write_all(&buffer[..n]) {
                    warn!("Write error: {}. Closing connection.", e);
                    break;
                }
            }
            Err(e) => {
                warn!("Read error: {}. Closing connection.", e);
                break;
            }
        }
    }

    read_stream.shutdown(Shutdown::Read).ok();
    write_stream.shutdown(Shutdown::Write).ok();
}

fn peek_stream(read_stream: &TcpStream) -> Result<String, Error> {
    let mut peek_buffer = vec![0; 1024]; // Reduced size for efficiency
    let bytes_peeked = read_stream.peek(&mut peek_buffer)?;
    let data = &peek_buffer[..bytes_peeked];
    Ok(String::from_utf8_lossy(data).to_string())
}

fn determine_proxy(client_stream: &mut TcpStream) -> Result<String, Error> {
    let addr_proxy = if let Ok(data_str) = peek_stream(client_stream) {
        if is_websocket(&data_str) {
            info!("Detected WebSocket connection.");
            get_http_proxy_address()
        } else if is_ssh(&data_str) {
            info!("Detected SSH connection.");
            get_ssh_address()
        } else if is_openvpn(&data_str) {
            info!("Detected OpenVPN connection.");
            get_openvpn_address()
        } else {
            warn!("Unknown traffic, defaulting to OpenVPN proxy.");
            get_openvpn_address()
        }
    } else {
        warn!("Failed to read data from client. Defaulting to OpenVPN proxy.");
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
            Ok(stream) => {
                info!("Successfully connected to proxy: {}", addr_proxy);
                return Ok(stream);
            }
            Err(e) if retries < max_retries => {
                warn!(
                    "Failed to connect to proxy {}. Retrying in {} seconds...",
                    addr_proxy,
                    delay.as_secs()
                );
                thread::sleep(delay);
                retries += 1;
                delay *= 2;
            }
            Err(e) => {
                error!(
                    "Failed to connect to proxy {} after {} attempts: {}",
                    addr_proxy, retries, e
                );
                return Err(e);
            }
        }
    }
}

fn get_port() -> u16 {
    let args: Vec<String> = env::args().collect();
    args.windows(2)
        .find(|w| w[0] == "--port")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(80)
}

fn get_status() -> String {
    let args: Vec<String> = env::args().collect();
    args.windows(2)
        .find(|w| w[0] == "--status")
        .map(|w| w[1].clone())
        .unwrap_or_else(|| String::from("@RustyManager"))
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
