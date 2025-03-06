use std::io::{Error, ErrorKind, Read, Result, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{env, thread};

// Configurações do proxy
#[derive(Clone)]
struct ProxyConfig {
    bind_addr: String,
    port: u16,
    status: String,
    ssh_redirect: SocketAddr,
    openvpn_redirect: SocketAddr,
    timeout: Duration,
    max_connections: usize,
    auth_token: Option<String>,
}

impl ProxyConfig {
    fn from_args() -> Result<Self> {
        let args: Vec<String> = env::args().collect();
        let mut config = ProxyConfig {
            bind_addr: "127.0.0.1".to_string(),
            port: 8080,
            status: "RustyProxy".to_string(),
            ssh_redirect: "127.0.0.1:22".parse().unwrap(),
            openvpn_redirect: "127.0.0.1:1194".parse().unwrap(),
            timeout: Duration::from_secs(5),
            max_connections: 100,
            auth_token: None,
        };

        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--port" => {
                    config.port = next_arg(&args, &mut i)?;
                }
                "--status" => {
                    config.status = next_arg(&args, &mut i)?;
                }
                "--bind" => {
                    config.bind_addr = next_arg(&args, &mut i)?;
                }
                "--ssh-redirect" => {
                    config.ssh_redirect = next_arg(&args, &mut i)?.parse().map_err(|_| {
                        Error::new(ErrorKind::InvalidInput, "Invalid SSH redirect address")
                    })?;
                }
                "--timeout" => {
                    let secs: u64 = next_arg(&args, &mut i)?;
                    config.timeout = Duration::from_secs(secs);
                }
                "--max-conn" => {
                    config.max_connections = next_arg(&args, &mut i)?;
                }
                "--auth-token" => {
                    config.auth_token = Some(next_arg(&args, &mut i)?);
                }
                _ => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!("Unknown argument: {}", args[i]),
                    ))
                }
            }
            i += 1;
        }
        Ok(config)
    }
}

fn next_arg<T: std::str::FromStr>(args: &[String], i: &mut usize) -> Result<T> {
    *i += 1;
    args.get(*i)
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "Missing argument value"))?
        .parse()
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "Invalid argument value"))
}

struct ConnectionManager {
    active_connections: usize,
    last_reset: Instant,
}

impl ConnectionManager {
    fn new(max_connections: usize) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            active_connections: 0,
            last_reset: Instant::now(),
        }))
    }

    fn check_connection(&mut self, max: usize) -> bool {
        if self.last_reset.elapsed() > Duration::from_secs(60) {
            self.active_connections = 0;
            self.last_reset = Instant::now();
        }
        
        if self.active_connections < max {
            self.active_connections += 1;
            true
        } else {
            false
        }
    }
}

fn main() -> Result<()> {
    env_logger::init();
    let config = ProxyConfig::from_args()?;
    let listener = TcpListener::bind(format!("{}:{}", config.bind_addr, config.port))?;
    
    log::info!(
        "Proxy iniciado em {}:{} | Timeout: {:?} | Conexões máximas: {}",
        config.bind_addr,
        config.port,
        config.timeout,
        config.max_connections
    );

    let conn_manager = ConnectionManager::new(config.max_connections);
    
    for stream in listener.incoming() {
        let config = config.clone();
        let conn_manager = conn_manager.clone();
        
        thread::spawn(move || {
            let mut client_stream = match stream {
                Ok(s) => s,
                Err(e) => {
                    log::error!("Erro na conexão: {}", e);
                    return;
                }
            };

            // Controle de conexões simultâneas
            {
                let mut manager = conn_manager.lock().unwrap();
                if !manager.check_connection(config.max_connections) {
                    log::warn!("Limite de conexões atingido");
                    let _ = client_stream.shutdown(Shutdown::Both);
                    return;
                }
            }

            if let Err(e) = handle_client(&mut client_stream, &config) {
                log::error!("Erro no cliente: {}", e);
            }
        });
    }
    Ok(())
}

fn handle_client(client_stream: &mut TcpStream, config: &ProxyConfig) -> Result<()> {
    // Verificação inicial de autenticação
    if let Some(ref token) = config.auth_token {
        let initial_data = peek_stream(client_stream, config.timeout)?;
        if !initial_data.contains(token) {
            client_stream.write_all(b"HTTP/1.1 401 Unauthorized\r\n\r\n")?;
            return Err(Error::new(ErrorKind::PermissionDenied, "Falha na autenticação"));
        }
    }

    // Detecção de protocolo melhorada
    let protocol = detect_protocol(client_stream, config.timeout)?;
    
    let target_addr = match protocol {
        Protocol::SSH => config.ssh_redirect,
        Protocol::OpenVPN => config.openvpn_redirect,
        Protocol::WebSocket => {
            perform_websocket_handshake(client_stream, &config.status)?;
            return Ok(());
        }
    };

    log::info!("Redirecionando para {} ({:?})", target_addr, protocol);
    
    let mut server_stream = TcpStream::connect_timeout(&target_addr, config.timeout)?;
    server_stream.set_read_timeout(Some(config.timeout))?;
    server_stream.set_write_timeout(Some(config.timeout))?;

    let (mut client_read, mut client_write) = (client_stream.try_clone()?, client_stream.try_clone()?);
    let (mut server_read, mut server_write) = (server_stream.try_clone()?, server_stream);

    let handle1 = thread::spawn(move || {
        transfer_data(&mut client_read, &mut server_write, config.timeout)
    });

    let handle2 = thread::spawn(move || {
        transfer_data(&mut server_read, &mut client_write, config.timeout)
    });

    handle1.join().unwrap()?;
    handle2.join().unwrap()?;

    Ok(())
}

#[derive(Debug)]
enum Protocol {
    SSH,
    OpenVPN,
    WebSocket,
}

fn detect_protocol(stream: &TcpStream, timeout: Duration) -> Result<Protocol> {
    let data = peek_stream(stream, timeout)?;
    
    // Detecção de SSH (versão do protocolo)
    if data.starts_with("SSH-") {
        return Ok(Protocol::SSH);
    }
    
    // Detecção de WebSocket
    if data.contains("Upgrade: websocket") && data.contains("Sec-WebSocket-Key") {
        return Ok(Protocol::WebSocket);
    }
    
    // Detecção de HTTP básico
    if data.starts_with("GET") || data.starts_with("POST") || data.starts_with("HTTP") {
        return Ok(Protocol::OpenVPN);
    }

    // Fallback para OpenVPN
    Ok(Protocol::OpenVPN)
}

fn perform_websocket_handshake(stream: &mut TcpStream, status: &str) -> Result<()> {
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\r\n",
        status
    );
    stream.write_all(response.as_bytes())?;
    Ok(())
}

fn transfer_data(read: &mut TcpStream, write: &mut TcpStream, timeout: Duration) -> Result<()> {
    let mut buffer = [0; 4096];
    loop {
        match read.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                write.write_all(&buffer[..n])?;
                write.flush()?;
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    write.shutdown(Shutdown::Both)?;
    Ok(())
}

fn peek_stream(stream: &TcpStream, timeout: Duration) -> Result<String> {
    let start_time = Instant::now();
    let mut buffer = [0; 1024];
    
    while start_time.elapsed() < timeout {
        match stream.peek(&mut buffer) {
            Ok(n) if n > 0 => {
                return Ok(String::from_utf8_lossy(&buffer[..n]).to_string());
            }
            _ => thread::sleep(Duration::from_millis(50)),
        }
    }
    
    Err(Error::new(ErrorKind::TimedOut, "Timeout ao ler dados"))
}
