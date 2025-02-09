use std::io::{Error, ErrorKind, Read, Result, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::time::Duration;
use std::{env, thread};

const MAX_BUFFER_SIZE: usize = 8192;

fn main() {
    let port = get_port();
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).unwrap_or_else(|e| {
        eprintln!("Falha ao iniciar listener: {}", e);
        std::process::exit(1);
    });
    println!("Proxy iniciado na porta {}", port);
    start_proxy(listener);
}

fn start_proxy(listener: TcpListener) {
    for stream in listener.incoming() {
        match stream {
            Ok(client_stream) => {
                thread::spawn(move || {
                    if let Err(e) = handle_client(client_stream) {
                        eprintln!("Erro no cliente: {}", e);
                    }
                });
            }
            Err(e) => {
                eprintln!("Erro ao aceitar conexão: {}", e);
            }
        }
    }
}

fn handle_client(mut client_stream: TcpStream) -> Result<()> {
    client_stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    client_stream.set_write_timeout(Some(Duration::from_secs(30)))?;

    // Ler dados iniciais sem consumir
    let (data_str, bytes_peeked) = peek_stream(&client_stream)?;
    let is_http = data_str.contains("HTTP");
    let is_websocket = data_str.contains("websocket") || data_str.contains("Upgrade: websocket");

    // Handshake WebSocket se necessário
    if is_http && is_websocket {
        let response = "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n";
        client_stream.write_all(response.as_bytes())?;
    }

    // Determinar proxy alvo
    let addr_proxy = determine_proxy(&data_str)?;

    // Conectar ao servidor alvo com retentativas
    let mut server_stream = attempt_connection_with_backoff(&addr_proxy)?;
    server_stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    server_stream.set_write_timeout(Some(Duration::from_secs(30)))?;

    // Consumir dados já inspecionados
    let mut consumed_buffer = vec![0; bytes_peeked];
    client_stream.read_exact(&mut consumed_buffer)?;
    server_stream.write_all(&consumed_buffer)?;

    // Clonar streams para comunicação bidirecional
    let (mut client_read, mut client_write) = (client_stream.try_clone()?, client_stream.try_clone()?);
    let (mut server_read, mut server_write) = (server_stream.try_clone()?, server_stream);

    // Threads para transferência de dados
    let client_to_server = thread::spawn(move || {
        transfer_data(&mut client_read, &mut server_write, "Cliente -> Servidor");
    });

    let server_to_client = thread::spawn(move || {
        transfer_data(&mut server_read, &mut client_write, "Servidor -> Cliente");
    });

    client_to_server.join().ok();
    server_to_client.join().ok();

    Ok(())
}

fn transfer_data(read_stream: &mut TcpStream, write_stream: &mut TcpStream, label: &str) {
    let mut buffer = [0; MAX_BUFFER_SIZE];
    loop {
        match read_stream.read(&mut buffer) {
            Ok(0) => {
                println!("{}: Conexão encerrada", label);
                break;
            }
            Ok(n) => {
                if let Err(e) = write_stream.write_all(&buffer[..n]) {
                    eprintln!("{}: Erro de escrita - {}", label, e);
                    break;
                }
                write_stream.flush().ok();
            }
            Err(e) => {
                eprintln!("{}: Erro de leitura - {}", label, e);
                break;
            }
        }
    }
    let _ = read_stream.shutdown(Shutdown::Read);
    let _ = write_stream.shutdown(Shutdown::Write);
}

fn peek_stream(stream: &TcpStream) -> Result<(String, usize)> {
    let mut buffer = [0; 1024];
    let bytes_peeked = stream.peek(&mut buffer)?;
    Ok((
        String::from_utf8_lossy(&buffer[..bytes_peeked]).into_owned(),
        bytes_peeked,
    ))
}

fn determine_proxy(data: &str) -> Result<String> {
    if data.starts_with("SSH") {
        Ok(get_ssh_address())
    } else if is_tls_handshake(data.as_bytes()) {
        Ok(get_openvpn_address())
    } else {
        Ok(get_http_proxy_address())
    }
}

fn is_tls_handshake(data: &[u8]) -> bool {
    data.len() >= 3 &&
    data[0] == 0x16 && // Tipo Handshake
    data[1] == 0x03 && // Versão TLS 1.x
    data[2] <= 0x03    // Subversão (1.0-1.3)
}

fn attempt_connection_with_backoff(addr: &str) -> Result<TcpStream> {
    let mut retries = 0;
    let max_retries = 5;
    let mut delay = Duration::from_secs(1);

    loop {
        match TcpStream::connect(addr) {
            Ok(stream) => return Ok(stream),
            Err(e) if retries < max_retries => {
                eprintln!("Conexão falhou ({}), tentando novamente em {}s...", e, delay.as_secs());
                thread::sleep(delay);
                retries += 1;
                delay *= 2;
            }
            Err(e) => return Err(Error::new(ErrorKind::ConnectionRefused, e)),
        }
    }
}

// Funções de configuração
fn get_port() -> u16 {
    env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "--port")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(80)
}

fn get_ssh_address() -> String {
    env::var("SSH_PROXY_ADDR").unwrap_or_else(|_| "127.0.0.1:22".into())
}

fn get_openvpn_address() -> String {
    env::var("OPENVPN_PROXY_ADDR").unwrap_or_else(|_| "127.0.0.1:1194".into())
}

fn get_http_proxy_address() -> String {
    env::var("HTTP_PROXY_ADDR").unwrap_or_else(|_| "127.0.0.1:80".into())
}
