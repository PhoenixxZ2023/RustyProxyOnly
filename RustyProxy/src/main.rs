use std::io::{Error, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;
use std::{env, thread};

fn main() {
    let port = get_port();
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).unwrap();
    start_http(listener);
}

fn start_http(listener: TcpListener) {
    for stream in listener.incoming() {
        match stream {
            Ok(client_stream) => {
                thread::spawn(move || {
                    handle_client(client_stream);
                });
            }
            Err(e) => eprintln!("Error accepting connection: {}", e),
        }
    }
}

fn handle_client(mut client_stream: TcpStream) {
    // Peek initial data to determine protocol
    let initial_data = match peek_stream(&client_stream) {
        Ok(data) => data,
        Err(_) => return,
    };

    if initial_data.starts_with("HTTP") {
        handle_http(&mut client_stream, initial_data);
    } else if initial_data.starts_with("SSH-") {
        proxy_to(&mut client_stream, "0.0.0.0:22");
    } else {
        proxy_to(&mut client_stream, "0.0.0.0:1194");
    }
}

fn handle_http(client_stream: &mut TcpStream, initial_data: String) {
    let status = get_status();
    let is_websocket = initial_data.to_lowercase().contains("websocket") || initial_data.to_lowercase().contains("upgrade: ws");

    if is_websocket {
        // Send proper WebSocket upgrade response
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
            Upgrade: websocket\r\n\
            Connection: Upgrade\r\n\
            Sec-WebSocket-Accept: {}\r\n\r\n",
            status // Note: Sec-WebSocket-Accept should be a valid hash, this is simplified
        );
        if client_stream.write_all(response.as_bytes()).is_err() {
            return;
        }
    } else {
        // Send generic HTTP response
        if client_stream.write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes()).is_err() {
            return;
        }
    }

    // After handling HTTP, decide backend (example: proxy to OpenVPN)
    proxy_to(client_stream, "0.0.0.0:1194");
}

fn proxy_to(client_stream: &mut TcpStream, addr: &str) {
    let server_stream = match TcpStream::connect(addr) {
        Ok(s) => s,
        Err(_) => return,
    };

    let (mut client_read, mut client_write) = (client_stream.try_clone().unwrap(), client_stream.try_clone().unwrap());
    let (mut server_read, mut server_write) = (server_stream.try_clone().unwrap(), server_stream);

    thread::spawn(move || {
        transfer_data(&mut client_read, &mut server_write);
    });

    thread::spawn(move || {
        transfer_data(&mut server_read, &mut client_write);
    });
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
    let _ = write_stream.shutdown(Shutdown::Both);
}

fn peek_stream(stream: &TcpStream) -> Result<String, Error> {
    let mut buffer = vec![0; 1024];
    let bytes_peeked = stream.peek(&mut buffer)?;
    Ok(String::from_utf8_lossy(&buffer[..bytes_peeked]).to_string())
}

fn get_port() -> u16 {
    env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(80)
}

fn get_status() -> String {
    env::args().nth(2).unwrap_or_else(|| "OK".into())
}
