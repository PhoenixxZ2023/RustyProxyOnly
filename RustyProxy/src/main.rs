use std::io::{Error, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;
use std::{env, thread};

fn main() {
    let port = get_port();
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port))
        .expect("Erro ao iniciar o servidor. Verifique se a porta está em uso.");

    println!("Proxy rodando na porta {}", port);
    start_http(listener);
}

fn start_http(listener: TcpListener) {
    for stream in listener.incoming() {
        match stream {
            Ok(mut client_stream) => {
                thread::spawn(move || {
                    handle_client(&mut client_stream);
                });
            }
            Err(e) => {
                eprintln!("Erro ao aceitar conexão: {}", e);
            }
        }
    }
}

fn handle_client(client_stream: &mut TcpStream) {
    let status = get_status();
    if client_stream.write_all(format!("HTTP/1.1 101 {}\r\n\r\n", status).as_bytes()).is_err() {
        return;
    }

    let data_str = match peek_stream(client_stream) {
        Ok(data) => data,
        Err(_) => return,
    };

    let addr_proxy = if data_str.contains("SSH") {
        "0.0.0.0:22"
    } else {
        "0.0.0.0:1194"
    };

    let server_connect = TcpStream::connect(addr_proxy);
    let server_stream = match server_connect {
        Ok(stream) => stream,
        Err(_) => return,
    };

    let client_read = client_stream.try_clone().unwrap();
    let client_write = client_stream.try_clone().unwrap();
    let server_read = server_stream.try_clone().unwrap();
    let server_write = server_stream;

    thread::spawn(move || {
        transfer_data(client_read, server_write);
    });

    thread::spawn(move || {
        transfer_data(server_read, client_write);
    });
}

fn transfer_data(mut read_stream: TcpStream, mut write_stream: TcpStream) {
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

fn peek_stream(read_stream: &TcpStream) -> Result<String, Error> {
    let mut peek_buffer = vec![0; 1024];
    let bytes_peeked = read_stream.peek(&mut peek_buffer)?;
    Ok(String::from_utf8_lossy(&peek_buffer[..bytes_peeked]).to_string())
}

fn get_port() -> u16 {
    env::args()
        .skip_while(|arg| arg != "--port")
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(80)
}

fn get_status() -> String {
    env::args()
        .skip_while(|arg| arg != "--status")
        .nth(1)
        .unwrap_or_else(|| "@RustyManager".to_string())
}
