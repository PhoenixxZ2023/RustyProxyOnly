use std::io::{Error, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::time::Duration;
use std::{env, thread};

const MAX_BUFFER_SIZE: usize = 8192;

fn main() {
    let port = get_port();
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).unwrap_or_else(|e| {
        eprintln!("Failed to start listener: {}", e);
        std::process::exit(1);
    });
    println!("Proxy started on port {}", port);
    start_http(listener);
}

fn start_http(listener: TcpListener) {
    for stream in listener.incoming() {
        match stream {
            Ok(client_stream) => {
                thread::spawn(move || {
                    if let Err(e) = handle_client(client_stream) {
                        eprintln!("Client handling error: {}", e);
                    }
                });
            }
            Err(e) => {
                eprintln!("Connection accept error: {}", e);
            }
        }
    }
}

fn handle_client(mut client_stream: TcpStream) -> Result<(), Error> {
    client_stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    client_stream.set_write_timeout(Some(Duration::from_secs(30)))?;

    let data_str = peek_stream(&client_stream)?;
    let is_http = data_str.contains("HTTP");
    let is_websocket = data_str.contains("websocket") || data_str.contains("Upgrade: websocket");

    if is_http {
        if is_websocket {
            // Perform WebSocket handshake
            let response = "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n";
            client_stream.write_all(response.as_bytes())?;
        } else {
            // Handle regular HTTP
            let status = get_status();
            client_stream.write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes())?;
        }
    }

    let addr_proxy = determine_proxy(&data_str)?;

    let mut server_stream = attempt_connection_with_backoff(&addr_proxy)?;
    server_stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    server_stream.set_write_timeout(Some(Duration::from_secs(30)))?;

    // Split streams after handshake
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
            Ok(0) => break, // Connection closed
            Ok(n) => {
                if let Err(e) = write_stream.write_all(&buffer[..n]) {
                    eprintln!("Write error: {}", e);
                    break;
                }
            }
            Err(e) => {
                eprintln!("Read error: {}", e);
                break;
            }
        }
    }

    // Gracefully shutdown
    let _ = read_stream.shutdown(Shutdown::Read);
    let _ = write_stream.shutdown(Shutdown::Write);
}

fn peek_stream(stream: &TcpStream) -> Result<String, Error> {
    let mut peek_buffer = [0; 1024];
    let bytes_peeked = stream.peek(&mut peek_buffer)?;
    Ok(String::from_utf8_lossy(&peek_buffer[..bytes_peeked]).into_owned())
}

fn determine_proxy(data: &str) -> Result<String, Error> {
    if data.starts_with("SSH") {
        Ok(get_ssh_address())
    } else if data.contains("OpenVPN") {
        Ok(get_openvpn_address())
    } else {
        // Default to HTTP proxy or other
        Ok(get_http_proxy_address())
    }
}

fn attempt_connection_with_backoff(addr: &str) -> Result<TcpStream, Error> {
    let mut retries = 0;
    let max_retries = 5;
    let mut delay = Duration::from_secs(1);

    loop {
        match TcpStream::connect(addr) {
            Ok(stream) => return Ok(stream),
            Err(e) if retries < max_retries => {
                eprintln!("Connection to {} failed ({}), retrying in {}s...", addr, e, delay.as_secs());
                thread::sleep(delay);
                retries += 1;
                delay *= 2;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn get_port() -> u16 {
    env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "--port")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(80)
}

fn get_status() -> String {
    env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "--status")
        .map(|w| w[1].clone())
        .unwrap_or_else(|| "@RustyManager".into())
}

fn get_ssh_address() -> String {
    env::var("SSH_PROXY_ADDR").unwrap_or_else(|_| "127.0.0.1:22".into())
}

fn get_openvpn_address() -> String {
    env::var("OPENVPN_PROXY_ADDR").unwrap_or_else(|_| "127.0.0.1:1194".into())
}

fn get_http_proxy_address() -> String {
    env::var("HTTP_PROXY_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into())
}
