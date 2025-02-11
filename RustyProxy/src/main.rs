use std::io::{self, Error, ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::time::Duration;
use std::{env, thread};

const BUFFER_SIZE: usize = 8192;
const READ_TIMEOUT: Duration = Duration::from_secs(5);

fn main() -> io::Result<()> {
    let port = get_port();
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port))?;
    println!("Proxy iniciado na porta {}", port);
    
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                thread::spawn(move || {
                    if let Err(e) = handle_client(stream) {
                        eprintln!("Erro no cliente: {}", e);
                    }
                });
            }
            Err(e) => eprintln!("Erro na conexão: {}", e),
        }
    }
    Ok(())
}

fn handle_client(mut client: TcpStream) -> io::Result<()> {
    client.set_read_timeout(Some(READ_TIMEOUT))?;
    client.set_write_timeout(Some(READ_TIMEOUT))?;

    // Fase 1: Detecção do protocolo
    let protocol = detect_protocol(&client)?;
    
    // Fase 2: Manipulação específica do protocolo
    match protocol {
        Protocol::HTTP => handle_http(&mut client)?,
        Protocol::WebSocket => handle_websocket(&mut client)?,
        Protocol::SSH => proxy_traffic(client, "0.0.0.0:22")?,
        Protocol::OpenVPN => proxy_traffic(client, "0.0.0.0:1194")?,
    }
    
    Ok(())
}

fn detect_protocol(stream: &TcpStream) -> io::Result<Protocol> {
    let mut buffer = [0; 1024];
    let bytes = stream.peek(&mut buffer)?;
    
    if bytes == 0 {
        return Err(Error::new(ErrorKind::TimedOut, "Timeout na detecção"));
    }

    let data = &buffer[..bytes];
    if data.starts_with(b"SSH-") {
        Ok(Protocol::SSH)
    } else if data.starts_with(b"GET ") || data.starts_with(b"POST ") {
        Ok(Protocol::HTTP)
    } else if data.windows(9).any(|w| w == b"Upgrade: ") {
        Ok(Protocol::WebSocket)
    } else {
        Ok(Protocol::OpenVPN)
    }
}

fn handle_http(client: &mut TcpStream) -> io::Result<()> {
    let status = get_status();
    let response = format!(
        "HTTP/1.1 200 {}\r\n\
        Content-Length: 0\r\n\r\n", 
        status
    );
    client.write_all(response.as_bytes())?;
    client.shutdown(Shutdown::Write)?;
    Ok(())
}

fn handle_websocket(client: &mut TcpStream) -> io::Result<()> {
    let response = 
        "HTTP/1.1 101 Switching Protocols\r\n\
        Upgrade: websocket\r\n\
        Connection: Upgrade\r\n\r\n";
    client.write_all(response.as_bytes())?;
    Ok(())
}

fn proxy_traffic(mut client: TcpStream, backend: &str) -> io::Result<()> {
    let mut server = TcpStream::connect(backend)?;
    server.set_read_timeout(Some(READ_TIMEOUT))?;
    server.set_write_timeout(Some(READ_TIMEOUT))?;

    let (mut client_reader, mut client_writer) = (client.try_clone()?, client.try_clone()?);
    let (mut server_reader, mut server_writer) = (server.try_clone()?, server);

    let client_to_server = thread::spawn(move || {
        copy_data(&mut client_reader, &mut server_writer);
    });

    let server_to_client = thread::spawn(move || {
        copy_data(&mut server_reader, &mut client_writer);
    });

    client_to_server.join().unwrap();
    server_to_client.join().unwrap();
    Ok(())
}

fn copy_data(source: &mut TcpStream, dest: &mut TcpStream) {
    let mut buffer = [0; BUFFER_SIZE];
    loop {
        match source.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                if let Err(e) = dest.write_all(&buffer[..n]) {
                    eprintln!("Erro de escrita: {}", e);
                    break;
                }
            }
            Err(e) => {
                eprintln!("Erro de leitura: {}", e);
                break;
            }
        }
    }
    let _ = dest.shutdown(Shutdown::Both);
}

#[derive(Debug)]
enum Protocol {
    HTTP,
    WebSocket,
    SSH,
    OpenVPN,
}

// Funções auxiliares (get_port, get_status mantidas similares)
