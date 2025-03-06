use std::io::{self, ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;
use std::{env, thread};

const TRANSFER_BUFFER_SIZE: usize = 8192;
const PEEK_BUFFER_SIZE: usize = 1024;
const PROTOCOL_TIMEOUT: u64 = 2;

fn main() -> io::Result<()> {
    let port = get_port().unwrap_or(80);
    let status = get_status();
    
    println!("{} iniciado na porta: {}", status, port);
    
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let status = status.clone();
                thread::spawn(move || {
                    if let Err(e) = handle_client(stream, &status) {
                        eprintln!("Erro na conexão: {}", e);
                    }
                });
            }
            Err(e) => eprintln!("❌ Conexão falhou: {}", e),
        }
    }
    Ok(())
}

fn handle_client(mut client: TcpStream, status: &str) -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    let client_clone = client.try_clone()?;
    
    thread::spawn(move || {
        tx.send(read_initial_data(&client_clone)).ok();
    });
    
    let (initial_data, bytes_read) = match rx.recv_timeout(Duration::from_secs(PROTOCOL_TIMEOUT)) {
        Ok(Ok((data, len))) => (data, len),
        _ => (String::new(), 0),
    };
    
    let (target, is_websocket) = detect_protocol(&initial_data);
    println!("🔀 Redirecionando para: {}", target);
    
    if target == "0.0.0.0:80" && status != "🚀 Proxy Rust v1.0" && !is_websocket {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            status.len(),
            status
        );
        client.write_all(response.as_bytes())?;
        client.shutdown(Shutdown::Both)?;
        return Ok(());
    }
    
    let mut server = TcpStream::connect(target)?;
    
    if bytes_read > 0 {
        server.write_all(&initial_data.as_bytes()[..bytes_read])?;
    }
    
    let (mut client_read, mut client_write) = (client.try_clone()?, client.try_clone()?);
    let (mut server_read, mut server_write) = (server.try_clone()?, server);
    
    let client_to_server = thread::spawn(move || {
        transfer_data(&mut client_read, &mut server_write, "Cliente → Servidor")
    });
    
    let server_to_client = thread::spawn(move || {
        transfer_data(&mut server_read, &mut client_write, "Servidor → Cliente")
    });
    
    client_to_server.join().unwrap()?;
    server_to_client.join().unwrap()?;
    
    Ok(())
}

fn detect_protocol(data: &str) -> (&str, bool) {
    let is_websocket = data.contains("Upgrade: websocket");
    
    if data.starts_with("SSH-") {
        ("0.0.0.0:22", false)
    } else if data.starts_with("CONNECT") {
        ("0.0.0.0:443", false)
    } else if data.starts_with("GET") || data.starts_with("POST") || data.starts_with("HTTP/") {
        if is_websocket {
            ("0.0.0.0:80", true)
        } else {
            ("0.0.0.0:8080", false)
        }
    } else {
        ("0.0.0.0:1194", false)
    }
}

fn transfer_data(
    read: &mut TcpStream,
    write: &mut TcpStream,
    direction: &str,
) -> io::Result<()> {
    let mut buffer = [0u8; TRANSFER_BUFFER_SIZE];
    loop {
        match read.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                write.write_all(&buffer[..n])?;
                write.flush()?;
                println!("{}: {} bytes transferidos", direction, n);
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        }
    }
    write.shutdown(Shutdown::Both)?;
    Ok(())
}

fn read_initial_data(stream: &TcpStream) -> io::Result<(String, usize)> {
    let mut buffer = [0u8; PEEK_BUFFER_SIZE];
    stream.set_read_timeout(Some(Duration::from_secs(PROTOCOL_TIMEOUT)))?;
    let bytes_read = stream.read(&mut buffer)?;
    stream.set_read_timeout(None)?;
    Ok((
        String::from_utf8_lossy(&buffer[..bytes_read]).into_owned(),
        bytes_read
    ))
}

fn get_port() -> Option<u16> {
    env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "--port")
        .and_then(|w| w[1].parse().ok())
}

fn get_status() -> String {
    env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "--status")
        .map(|w| w[1].clone())
        .unwrap_or_else(|| "🚀 Proxy Rust v1.0".into())
}
