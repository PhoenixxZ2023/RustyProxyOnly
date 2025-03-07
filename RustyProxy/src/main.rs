use std::io::{Error, ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
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
                    if let Err(e) = handle_client(client_stream) {
                        eprintln!("Client handling error: {}", e);
                    }
                });
            }
            Err(e) => {
                eprintln!("Error accepting connection: {}", e);
            }
        }
    }
}

fn handle_client(mut client_stream: TcpStream) -> Result<(), Error> {
    // Peek initial data to determine protocol
    let initial_data = peek_initial_data(&client_stream)?;

    // Determine backend based on initial data
    let (backend_addr, is_http) = determine_backend(&initial_data);

    // Connect to the determined backend
    let mut server_stream = TcpStream::connect(backend_addr)
        .map_err(|e| Error::new(ErrorKind::Other, format!("Failed to connect to backend: {}", e)))?;

    // If HTTP/WebSocket, handle handshake
    if is_http {
        handle_http_handshake(&mut client_stream, &initial_data)?;
        // Forward initial data to backend (already read)
        server_stream.write_all(&initial_data)?;
    } else {
        // For non-HTTP, forward initial data if any
        if !initial_data.is_empty() {
            server_stream.write_all(&initial_data)?;
        }
    }

    // Split streams and start bidirectional transfer
    let (mut client_read, mut client_write) = (client_stream.try_clone()?, client_stream.try_clone()?);
    let (mut server_read, mut server_write) = (server_stream.try_clone()?, server_stream);

    let client_to_server = thread::spawn(move || {
        transfer_data(&mut client_read, &mut server_write);
    });

    let server_to_client = thread::spawn(move || {
        transfer_data(&mut server_read, &mut client_write);
    });

    // Wait for both threads to finish
    let _ = client_to_server.join();
    let _ = server_to_client.join();

    Ok(())
}

fn determine_backend(initial_data: &[u8]) -> (String, bool) {
    let status = get_status();
    // Check for SSH
    if initial_data.starts_with(b"SSH-") {
        ("0.0.0.0:22".to_string(), false)
    }
    // Check for HTTP
    else if initial_data.starts_with(b"GET ") || initial_data.starts_with(b"POST ") || initial_data.starts_with(b"HTTP/") {
        ("0.0.0.0:80".to_string(), true) // Example HTTP backend
    }
    // Check for WebSocket handshake
    else if initial_data.windows(8).any(|w| w == b"Upgrade") && initial_data.windows(10).any(|w| w == b"WebSocket") {
        ("0.0.0.0:8080".to_string(), true) // Example WebSocket backend
    }
    // Default to OpenVPN
    else {
        ("0.0.0.0:1194".to_string(), false)
    }
}

fn handle_http_handshake(client_stream: &mut TcpStream, initial_data: &[u8]) -> Result<(), Error> {
    // Check if it's a WebSocket upgrade request
    let is_websocket = initial_data.contains(&b"Upgrade: websocket"[..]) || initial_data.contains(&b"Upgrade: ws"[..]);

    if is_websocket {
        // Send proper WebSocket upgrade response
        let response = "HTTP/1.1 101 Switching Protocols\r\n\
                       Upgrade: websocket\r\n\
                       Connection: Upgrade\r\n\
                       Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n";
        client_stream.write_all(response.as_bytes())?;
    } else {
        // For regular HTTP, send a generic response or forward to backend
        let status = get_status();
        let response = format!("HTTP/1.1 200 {}\r\n\r\n", status);
        client_stream.write_all(response.as_bytes())?;
    }
    Ok(())
}

fn peek_initial_data(stream: &TcpStream) -> Result<Vec<u8>, Error> {
    let mut buffer = vec![0; 1024];
    let bytes_read = stream.peek(&mut buffer)?;
    Ok(buffer[..bytes_read].to_vec())
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
    write_stream.shutdown(Shutdown::Both).ok();
}

// get_port and get_status functions remain similar to original
fn get_port() -> u16 {
    env::args().nth(1).and_then(|arg| arg.parse().ok()).unwrap_or(80)
}

fn get_status() -> String {
    env::args().find(|arg| arg.starts_with("--status="))
        .and_then(|arg| arg.splitn(2, '=').nth(1))
        .unwrap_or("OK").to_string()
}
