use std::io::{Error, ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;
use std::{env, thread};
use log::{error, info};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Inicializa o logger
    env_logger::init();
    
    let listener = TcpListener::bind(format!("0.0.0.0:{}", get_port()))
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
    info!("Proxy iniciado na porta {}", get_port());
    start_http(listener)?;
    Ok(())
}

/// Inicia o servidor proxy e aceita conexões
fn start_http(listener: TcpListener) -> Result<(), Error> {
    for stream in listener.incoming() {
        match stream {
            Ok(mut client_stream) => {
                info!("Nova conexão aceita de {:?}", client_stream.peer_addr());
                thread::spawn(move || {
                    if let Err(e) = handle_client(&mut client_stream) {
                        error!("Erro ao tratar cliente: {}", e);
                    }
                });
            }
            Err(e) => error!("Erro ao aceitar conexão: {}", e),
        }
    }
    Ok(())
}

/// Trata uma conexão individual de cliente
fn handle_client(client_stream: &mut TcpStream) -> Result<(), Error> {
    client_stream.set_nodelay(true)?;
    client_stream.set_read_timeout(Some(Duration::from_secs(30)))?;

    let status = get_status();
    client_stream.write_all(format!("HTTP/1.1 101 {}\r\n\r\n", status).as_bytes())?;

    let data_str = peek_stream(client_stream)?;
    
    // Determina o destino baseado no protocolo
    let target_addr = if data_str.contains("HTTP") {
        if data_str.to_lowercase().contains("websocket") || data_str.to_lowercase().contains("ws") {
            client_stream.write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes())?;
            return Ok(()); // Para simplificar, não implementamos handshake WebSocket completo
        }
        "0.0.0.0:1194" // Assume OpenVPN como default para não-SSH
    } else if data_str.contains("SSH") {
        "0.0.0.0:22"
    } else {
        "0.0.0.0:1194"
    };

    info!("Roteando para {}", target_addr);
    let mut server_stream = TcpStream::connect(target_addr)?;
    server_stream.set_nodelay(true)?;
    server_stream.set_read_timeout(Some(Duration::from_secs(30)))?;

    // Clona streams para transferência bidirecional
    let (mut client_read, mut client_write) = (client_stream.try_clone()?, client_stream.try_clone()?);
    let (mut server_read, mut server_write) = (server_stream.try_clone()?, server_stream);

    thread::spawn(move || {
        if let Err(e) = transfer_data(&mut client_read, &mut server_write) {
            error!("Erro na transferência client->server: {}", e);
        }
    });

    thread::spawn(move || {
        if let Err(e) = transfer_data(&mut server_read, &mut client_write) {
            error!("Erro na transferência server->client: {}", e);
        }
    });

    Ok(())
}

/// Transfere dados entre dois streams
fn transfer_data(read_stream: &mut TcpStream, write_stream: &mut TcpStream) -> Result<(), Error> {
    let mut buffer = [0; 2048];
    loop {
        match read_stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                write_stream.write_all(&buffer[..n])?;
                write_stream.flush()?;
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        }
    }
    write_stream.shutdown(Shutdown::Both)?;
    read_stream.shutdown(Shutdown::Both)?;
    Ok(())
}

/// Faz peek nos dados do stream sem consumi-los
fn peek_stream(read_stream: &TcpStream) -> Result<String, Error> {
    let mut peek_buffer = vec![0; 1024];
    let bytes_peeked = read_stream.peek(&mut peek_buffer)?;
    if bytes_peeked > 1024 {
        return Err(Error::new(ErrorKind::Other, "Payload muito grande"));
    }
    let data = &peek_buffer[..bytes_peeked];
    Ok(String::from_utf8_lossy(data).to_string())
}

/// Obtém a porta dos argumentos ou retorna default
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

/// Obtém o status dos argumentos ou retorna default
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
