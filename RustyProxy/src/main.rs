use std::io::{Error, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::time::Duration;
use std::{env, thread};

const BUFFER_SIZE: usize = 8192;
const TIMEOUT_SECONDS: u64 = 30;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = get_arg_value("--port").unwrap_or(80);
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port))?;
    println!("Proxy running on port {}", port);
    
    start_proxy(listener)?;
    Ok(())
}

fn start_proxy(listener: TcpListener) -> Result<(), Error> {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                thread::spawn(move || {
                    if let Err(e) = handle_connection(stream) {
                        eprintln!("Connection error: {}", e);
                    }
                });
            }
            Err(e) => eprintln!("Accept error: {}", e),
        }
    }
    Ok(())
}

fn handle_connection(mut client: TcpStream) -> Result<(), Error> {
    // Configure timeouts
    let timeout = Duration::from_secs(TIMEOUT_SECONDS);
    client.set_read_timeout(Some(timeout))?;
    client.set_write_timeout(Some(timeout))?;

    // Protocol detection
    let protocol = detect_protocol(&client)?;
    
    match protocol {
        Protocol::Http | Protocol::WebSocket => handle_http(client, protocol)?,
        Protocol::Ssh => proxy_traffic(client, get_ssh_address())?,
        Protocol::OpenVpn => proxy_traffic(client, get_openvpn_address())?,
    }
    
    Ok(())
}

fn detect_protocol(stream: &TcpStream) -> Result<Protocol, Error> {
    let mut buffer = [0; 1024];
    let bytes = stream.peek(&mut buffer)?;
    
    let data = &buffer[..bytes];
    if data.starts_with(b"SSH-") {
        Ok(Protocol::Ssh)
    } else if data.starts_with(b"GET ") || data.starts_with(b"POST ") || data.starts_with(b"HTTP") {
        if data.windows(11).any(|w| w == b"Upgrade: ") {
            Ok(Protocol::WebSocket)
        } else {
            Ok(Protocol::Http)
        }
    } else {
        Ok(Protocol::OpenVpn)
    }
}

fn handle_http(mut client: TcpStream, protocol: Protocol) -> Result<(), Error> {
    let status = get_arg_value("--status").unwrap_or_else(|| "OK".into());
    
    // Send appropriate HTTP response
    let response = match protocol {
        Protocol::WebSocket => format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
            Upgrade: websocket\r\n\
            Connection: Upgrade\r\n\r\n"
        ),
        _ => format!(
            "HTTP/1.1 200 {}\r\n\
            Content-Length: 0\r\n\r\n", 
            status
        ),
    };
    
    client.write_all(response.as_bytes())?;
    client.shutdown(Shutdown::Write)?;
    Ok(())
}

fn proxy_traffic(mut client: TcpStream, backend: String) -> Result<(), Error> {
    let mut server = TcpStream::connect(backend)?;
    server.set_read_timeout(Some(Duration::from_secs(TIMEOUT_SECONDS))?;
    server.set_write_timeout(Some(Duration::from_secs(TIMEOUT_SECONDS))?;

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
            Ok(0) => break, // EOF
            Ok(n) => {
                if let Err(e) = dest.write_all(&buffer[..n]) {
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
    let _ = dest.shutdown(Shutdown::Both);
}

// Helper functions
fn get_arg_value(name: &str) -> Option<String> {
    env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == name)
        .and_then(|w| Some(w[1].clone()))
}

fn get_ssh_address() -> String {
    env::var("SSH_PROXY_ADDR").unwrap_or_else(|_| "0.0.0.0:22".into())
}

fn get_openvpn_address() -> String {
    env::var("OPENVPN_PROXY_ADDR").unwrap_or_else(|_| "0.0.0.0:1194".into())
}

#[derive(Debug, PartialEq)]
enum Protocol {
    Http,
    WebSocket,
    Ssh,
    OpenVpn,
}
