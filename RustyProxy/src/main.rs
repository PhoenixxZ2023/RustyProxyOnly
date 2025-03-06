use std::io::{self, Error, ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;
use std::{env, thread};

const TRANSFER_BUFFER_SIZE: usize = 8192;
const PEEK_BUFFER_SIZE: usize = 1024;
const PROTOCOL_TIMEOUT: u64 = 2;

fn main() -> io::Result<()> {
    let port = get_port().unwrap_or(8080);
    let status = get_status();
    
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    println!("🚀 Proxy iniciado na porta: {}", port);
    
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
    
    // Thread para análise inicial do protocolo
    thread::spawn(move || {
        tx.send(peek_stream(&client_clone)).ok();
    });
    
    // Detecção de protocolo com timeout
    let initial_data = match rx.recv_timeout(Duration::from_secs(PROTOCOL_TIMEOUT)) {
        Ok(Ok(data)) => data,
        _ => String::new(),
    };
    
    // Determinar destino com base nos dados iniciais
    let target = detect_protocol(&initial_data);
    println!("🔀 Redirecionando para: {}", target);
    
    // Conexão com o servidor de destino
    let mut server = TcpStream::connect(target)?;
    
    // Handshake HTTP/WebSocket se necessário
    if is_http(&initial_data) {
        handle_http(&mut client, status, &initial_data)?;
    }
    
    // Transferência bidirecional de dados
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

fn detect_protocol(data: &str) -> &str {
    if data.starts_with("SSH-") {
        "0.0.0.0:22"
    } else if data.starts_with("GET") || data.starts_with("POST") || data.starts_with("HTTP/") {
        "0.0.0.0:80"
    } else {
        "0.0.0.0:1194" // OpenVPN
    }
}

fn handle_http(client: &mut TcpStream, status: &str, initial_data: &str) -> io::Result<()> {
    if initial_data.starts_with("GET") {
        let response = if is_websocket(initial_data) {
            format!(
                "HTTP/1.1 101 Switching Protocols\r\n\
                Upgrade: websocket\r\n\
                Connection: Upgrade\r\n\
                Sec-WebSocket-Accept: {}\r\n\r\n",
                status
            )
        } else {
            format!(
                "HTTP/1.1 200 OK\r\n\
                Content-Length: {}\r\n\
                Connection: close\r\n\r\n\
                {}",
                status.len(),
                status
            )
        };
        
        client.write_all(response.as_bytes())?;
        client.flush()?;
    }
    Ok(())
}

fn is_websocket(data: &str) -> bool {
    data.contains("Upgrade: websocket") || 
    data.contains("Sec-WebSocket-Key")
}

fn transfer_data(
    read: &mut TcpStream,
    write: &mut TcpStream,
    direction: &str,
) -> io::Result<()> {
    let mut buffer = [0u8; TRANSFER_BUFFER_SIZE];
    loop {
        match read.read(&mut buffer) {
            Ok(0) => break, // Conexão fechada
            Ok(n) => {
                write.write_all(&buffer[..n])?;
                write.flush()?;
                println!("{} - {} bytes transferidos", direction, n);
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        }
    }
    write.shutdown(Shutdown::Both)?;
    Ok(())
}

fn peek_stream(stream: &TcpStream) -> io::Result<String> {
    let mut buffer = [0u8; PEEK_BUFFER_SIZE];
    let n = stream.peek(&mut buffer)?;
    Ok(String::from_utf8_lossy(&buffer[..n]).into_owned())
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
