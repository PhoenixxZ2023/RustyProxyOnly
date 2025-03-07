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
    let initial_data = peek_initial_data(&client_stream)?;
    let (backend_addr, is_http) = determine_backend(&initial_data);
    let mut server_stream = TcpStream::connect(backend_addr)
        .map_err(|e| Error::new(ErrorKind::Other, format!("Failed to connect to backend: {}", e)))?;

    if is_http {
        handle_http_handshake(&mut client_stream, &initial_data)?;
        server_stream.write_all(&initial_data)?;
    } else if !initial_data.is_empty() {
        server_stream.write_all(&initial_data)?;
    }

    let (mut client_read, mut client_write) = (client_stream.try_clone()?, client_stream.try_clone()?);
    let (mut server_read, mut server_write) = (server_stream.try_clone()?, server_stream);

    let client_to_server = thread::spawn(move || {
        transfer_data(&mut client_read, &mut server_write);
    });

    let server_to_client = thread::spawn(move || {
        transfer_data(&mut server_read, &mut client_write);
    });

    let _ = client_to_server.join();
    let _ = server_to_client.join();

    Ok(())
}

fn determine_backend(initial_data: &[u8]) -> (String, bool) {
    if initial_data.starts_with(b"SSH-") {
        ("0.0.0.0:22".to_string(), false)
    } else if initial_data.starts_with(b"GET ") || 
              initial_data.starts_with(b"POST ") || 
              initial_data.starts_with(b"HTTP/") {
        ("0.0.0.0:80".to_string(), true)
    } else if initial_data.windows(8).any(|w| w == b"Upgrade") && 
              initial_data.windows(10).any(|w| w == b"WebSocket") {
        ("0.0.0.0:8080".to_string(), true)
    } else {
        ("0.0.0.0:1194".to_string(), false)
    }
}

fn handle_http_handshake(client_stream: &mut TcpStream, initial_data: &[u8]) -> Result<(), Error> {
    let is_websocket = initial_data.contains(&b"Upgrade: websocket"[..]) || 
                      initial_data.contains(&b"Upgrade: ws"[..]);

    if is_websocket {
        client_stream.write_all(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n".as_bytes()
        )?;
    } else {
        client_stream.write_all("HTTP/1.1 200 OK\r\n\r\n".as_bytes())?;
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
            Ok(0) => break,
            Ok(n) => {
                if write_stream.write_all(&buffer[..n]).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    write_stream.shutdown(Shutdown::Both).ok();
}

fn get_port() -> u16 {
    env::args().nth(1).and_then(|arg| arg.parse().ok()).unwrap_or(80)
}
