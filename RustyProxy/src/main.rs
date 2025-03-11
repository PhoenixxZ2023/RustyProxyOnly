use std::io::{Error, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;
use std::thread;
use std::env;

fn main() {
    let listener = TcpListener::bind(format!("0.0.0.0:{}", get_port())).unwrap();
    println!("Servidor iniciado na porta {}", get_port());
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
    match peek_stream(client_stream) {
        Ok(data_str) => {
            if data_str.contains("HTTP") {
                handle_http(client_stream, &status);
            } else if data_str.contains("SSH") {
                proxy_to_backend(client_stream, "0.0.0.0:22");
            } else {
                proxy_to_backend(client_stream, "0.0.0.0:1194");
            }
        }
        Err(e) => {
            eprintln!("Erro ao inspecionar stream: {}", e);
        }
    }
}

fn handle_http(client_stream: &mut TcpStream, status: &str) {
    let mut buffer = vec![0; 1024];
    let bytes_read = client_stream.read(&mut buffer).unwrap_or(0);
    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
    if request.to_lowercase().contains("websocket") {
        client_stream.write_all(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n"
                .as_bytes(),
        ).ok();
    } else {
        client_stream.write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes()).ok();
    }
}

fn proxy_to_backend(client_stream: &mut TcpStream, addr_proxy: &str) {
    let server_connect = TcpStream::connect(addr_proxy);
    if let Err(e) = server_connect {
        eprintln!("Erro ao conectar ao backend {}: {}", addr_proxy, e);
        return;
    }
    let server_stream = server_connect.unwrap();
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
            Ok(0) => break, // Conexão fechada
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

fn peek_stream(read_stream: &TcpStream) -> Result<String, Error> {
    let mut peek_buffer = vec![0; 1024];
    let bytes_peeked = read_stream.peek(&mut peek_buffer)?;
    let data = &peek_buffer[..bytes_peeked];
    let data_str = String::from_utf8_lossy(data);
    Ok(data_str.to_string())
}

fn get_port() -> u16 {
    let args: Vec<String> = env::args().collect();
    let mut port = 80;
    for i in 1..args.len() {
        if args[i] == "--port" && i + 1 < args.len() {
            port = args[i + 1].parse().unwrap_or(80);
        }
    }
    port
}

fn get_status() -> String {
    let args: Vec<String> = env::args().collect();
    let mut status = String::from("@RustyManager");
    for i in 1..args.len() {
        if args[i] == "--status" && i + 1 < args.len() {
            status = args[i + 1].clone();
        }
    }
    status
}
