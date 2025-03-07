use std::io::{Error, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::time::Duration;
use std::{env, thread};
use sha1::{Sha1, Digest};
use base64::Engine;

// Configurações do proxy
struct ProxyConfig {
    listen_port: u16,
    ssh_backend: String,
    http_backend: String,
    ws_backend: String,
    openvpn_backend: String,
    custom_status: String,
}

impl ProxyConfig {
    fn from_args() -> Self {
        let args: Vec<String> = env::args().collect();
        
        ProxyConfig {
            listen_port: args.iter().position(|a| a == "--port")
                .and_then(|i| args.get(i + 1))
                .and_then(|p| p.parse().ok())
                .unwrap_or(80),
                
            ssh_backend: args.iter().position(|a| a == "--ssh")
                .and_then(|i| args.get(i + 1))
                .cloned()
                .unwrap_or_else(|| "0.0.0.0:22".into()),
                
            http_backend: args.iter().position(|a| a == "--http")
                .and_then(|i| args.get(i + 1))
                .cloned()
                .unwrap_or_else(|| "0.0.0.0:80".into()),
                
            ws_backend: args.iter().position(|a| a == "--ws")
                .and_then(|i| args.get(i + 1))
                .cloned()
                .unwrap_or_else(|| "0.0.0.0:8765".into()),
                
            openvpn_backend: args.iter().position(|a| a == "--openvpn")
                .and_then(|i| args.get(i + 1))
                .cloned()
                .unwrap_or_else(|| "0.0.0.0:1194".into()),
                
            custom_status: args.iter().position(|a| a == "--status")
                .and_then(|i| args.get(i + 1))
                .cloned()
                .unwrap_or_else(|| "RustyProxy".into()),
        }
    }
}

fn main() {
    let config = ProxyConfig::from_args();
    let listener = TcpListener::bind(format!("0.0.0.0:{}", config.listen_port)).unwrap();
    start_proxy(listener, config);
}

fn start_proxy(listener: TcpListener, config: ProxyConfig) {
    for stream in listener.incoming() {
        match stream {
            Ok(client_stream) => {
                let config = config.clone();
                thread::spawn(move || {
                    handle_client(client_stream, config);
                });
            }
            Err(e) => eprintln!("Error accepting connection: {}", e),
        }
    }
}

fn handle_client(mut client_stream: TcpStream, config: ProxyConfig) {
    let initial_data = match peek_stream(&client_stream, Duration::from_secs(1)) {
        Ok(data) => data,
        Err(_) => return,
    };

    if initial_data.starts_with("HTTP") {
        handle_http(&mut client_stream, &initial_data, &config);
    } else if initial_data.starts_with("SSH-") {
        proxy_to(&mut client_stream, &config.ssh_backend);
    } else {
        proxy_to(&mut client_stream, &config.openvpn_backend);
    }
}

fn handle_http(client_stream: &mut TcpStream, initial_data: &str, config: &ProxyConfig) {
    let is_websocket = initial_data.contains("Upgrade: websocket") 
        || initial_data.contains("Sec-WebSocket-Key:");

    if is_websocket {
        let client_key = initial_data
            .lines()
            .find(|line| line.starts_with("Sec-WebSocket-Key:"))
            .and_then(|line| line.split_once(':'))
            .map(|(_, value)| value.trim())
            .unwrap_or_default();

        let accept_key = compute_ws_accept(client_key);
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
            Upgrade: websocket\r\n\
            Connection: Upgrade\r\n\
            Sec-WebSocket-Accept: {}\r\n\r\n",
            accept_key
        );
        
        if client_stream.write_all(response.as_bytes()).is_ok() {
            proxy_to(client_stream, &config.ws_backend);
        }
    } else {
        let response = format!("HTTP/1.1 200 {}\r\n\r\n", config.custom_status);
        let _ = client_stream.write_all(response.as_bytes());
        proxy_to(client_stream, &config.http_backend);
    }
}

fn compute_ws_accept(key: &str) -> String {
    let mut sha1 = Sha1::new();
    sha1.update(key.as_bytes());
    sha1.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64::engine::general_purpose::STANDARD.encode(sha1.finalize())
}

fn proxy_to(client_stream: &mut TcpStream, backend: &str) {
    let Ok(mut server_stream) = TcpStream::connect(backend) else {
        eprintln!("Failed to connect to backend: {}", backend);
        return;
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
    let mut buffer = [0; 4096];
    loop {
        match read_stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => if write_stream.write_all(&buffer[..n]).is_err() {
                break;
            },
            Err(_) => break,
        }
    }
    let _ = write_stream.shutdown(Shutdown::Both);
}

fn peek_stream(stream: &TcpStream, timeout: Duration) -> Result<String, Error> {
    let mut buffer = vec![0; 1024];
    stream.set_read_timeout(Some(timeout))?;
    let bytes_peeked = stream.peek(&mut buffer)?;
    Ok(String::from_utf8_lossy(&buffer[..bytes_peeked]).into_owned())
}
