use std::io::{Error, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::time::Duration;
use std::{env, thread};

fn main() {
    let port = get_port();
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).unwrap();
    start_proxy(listener);
}

fn start_proxy(listener: TcpListener) {
    for stream in listener.incoming() {
        match stream {
            Ok(client_stream) => {
                thread::spawn(move || {
                    handle_client(client_stream);
                });
            }
            Err(e) => {
                eprintln!("Error accepting connection: {}", e);
            }
        }
    }
}

fn handle_client(mut client_stream: TcpStream) {
    let mut backend_addr = get_backend_addr(); // Default backend

    // Peek the initial data to determine protocol
    match peek_stream(&client_stream) {
        Ok(data_str) => {
            if data_str.starts_with("HTTP") {
                // Check for WebSocket upgrade
                if data_str.contains("Upgrade: websocket") {
                    if let Ok(ws_backend) = get_websocket_backend() {
                        backend_addr = ws_backend;
                        if let Err(e) = perform_websocket_handshake(&mut client_stream) {
                            eprintln!("WebSocket handshake failed: {}", e);
                            return;
                        }
                    }
                } else {
                    // Regular HTTP, send a response if configured
                    if let Err(e) = send_http_response(&mut client_stream) {
                        eprintln!("HTTP response failed: {}", e);
                        return;
                    }
                    backend_addr = get_http_backend();
                }
            } else if data_str.starts_with("SSH-") {
                backend_addr = get_ssh_backend();
            } else {
                backend_addr = get_openvpn_backend();
            }
        }
        Err(e) => {
            eprintln!("Peek failed: {}", e);
            return;
        }
    }

    // Connect to the determined backend
    let mut server_stream = match TcpStream::connect(&backend_addr) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to connect to backend {}: {}", backend_addr, e);
            return;
        }
    };

    // Split streams and proxy data
    let (mut client_read, mut client_write) = (client_stream.try_clone().unwrap(), client_stream);
    let (mut server_read, mut server_write) = (server_stream.try_clone().unwrap(), server_stream);

    let client_to_server = thread::spawn(move || {
        transfer_data(&mut client_read, &mut server_write);
    });

    let server_to_client = thread::spawn(move || {
        transfer_data(&mut server_read, &mut client_write);
    });

    let _ = client_to_server.join();
    let _ = server_to_client.join();
}

fn transfer_data(read_stream: &mut TcpStream, write_stream: &mut TcpStream) {
    let mut buffer = [0; 2048];
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
    let _ = write_stream.shutdown(Shutdown::Both);
}

fn peek_stream(stream: &TcpStream) -> Result<String, Error> {
    let mut buffer = [0; 1024];
    let bytes_peeked = stream.peek(&mut buffer)?;
    Ok(String::from_utf8_lossy(&buffer[..bytes_peeked]).to_string())
}

fn perform_websocket_handshake(stream: &mut TcpStream) -> Result<(), Error> {
    // This should parse Sec-WebSocket-Key and compute Sec-WebSocket-Accept
    let response = "HTTP/1.1 101 Switching Protocols\r\n\
                   Upgrade: websocket\r\n\
                   Connection: Upgrade\r\n\
                   Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n";
    stream.write_all(response.as_bytes())
}

fn send_http_response(stream: &mut TcpStream) -> Result<(), Error> {
    let status = get_status();
    let response = format!("HTTP/1.1 200 {}\r\nContent-Length: 0\r\n\r\n", status);
    stream.write_all(response.as_bytes())
}

// Functions to get backend addresses from command-line arguments
fn get_backend_addr() -> String {
    get_arg_value("--backend").unwrap_or_else(|| "0.0.0.0:22".to_string())
}

fn get_http_backend() -> String {
    get_arg_value("--http-backend").unwrap_or_else(|| "0.0.0.0:80".to_string())
}

fn get_websocket_backend() -> Result<String, &'static str> {
    get_arg_value("--ws-backend").ok_or("WebSocket backend not specified")
}

fn get_ssh_backend() -> String {
    get_arg_value("--ssh-backend").unwrap_or_else(|| "0.0.0.0:22".to_string())
}

fn get_openvpn_backend() -> String {
    get_arg_value("--ovpn-backend").unwrap_or_else(|| "0.0.0.0:1194".to_string())
}

fn get_arg_value(arg: &str) -> Option<String> {
    let args: Vec<String> = env::args().collect();
    args.iter().position(|a| a == arg).and_then(|i| args.get(i + 1)).cloned()
}

fn get_port() -> u16 {
    get_arg_value("--port").and_then(|p| p.parse().ok()).unwrap_or(80)
}

fn get_status() -> String {
    get_arg_value("--status").unwrap_or_else(|| "@RustyManager".to_string())
}
