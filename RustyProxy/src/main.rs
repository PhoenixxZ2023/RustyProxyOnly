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
                    }
                } else {
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

    // Clone client and server streams, handling potential errors
    let client_clone = match client_stream.try_clone() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to clone client stream: {}", e);
            return;
        }
    };
    let server_clone = match server_stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to clone server stream: {}", e);
            return;
        }
    };

    let (mut client_read, mut client_write) = (client_clone, client_stream);
    let (mut server_read, mut server_write) = (server_clone, server_stream);

    // Transfer data between client and server
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
