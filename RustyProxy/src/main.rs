use std::io::{Error, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;
use std::{env, thread};

const MAX_BUFFER_SIZE: usize = 8192;

fn main() {
    let listener = TcpListener::bind(format!("0.0.0.0:{}", get_port())).unwrap_or_else(|e| {
        eprintln!("Erro ao iniciar o listener: {}", e);
        std::process::exit(1);
    });
    println!("Proxy iniciado na porta {}", get_port());
    start_http(listener);
}

fn start_http(listener: TcpListener) {
    for stream in listener.incoming() {
        match stream {
            Ok(mut client_stream) => {
                thread::spawn(move || {
                    if let Err(e) = handle_client(&mut client_stream) {
                        eprintln!("Erro ao processar cliente: {}", e);
                    }
                });
            }
            Err(e) => {
                eprintln!("Erro ao aceitar conexão: {}", e);
            }
        }
    }
}

fn handle_client(client_stream: &mut TcpStream) -> Result<(), Error> {
    let status = get_status();

    client_stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    client_stream.set_write_timeout(Some(Duration::from_secs(30)))?;

    let data_str = peek_stream(client_stream)?;

    if data_str.contains("Upgrade: websocket") {
        perform_websocket_handshake(client_stream, &data_str)?;
    } else if data_str.contains("HTTP") {
        client_stream.write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes())?;
    }

    let addr_proxy = determine_proxy(client_stream)?;

    let server_stream = attempt_connection_with_backoff(&addr_proxy)?;

    let (mut client_read, mut client_write) = (client_stream.try_clone()?, client_stream.try_clone()?);
    let (mut server_read, mut server_write) = (server_stream.try_clone()?, server_stream);

    let client_to_server = thread::spawn(move || {
        transfer_data(&mut client_read, &mut server_write);
    });

    let server_to_client = thread::spawn(move || {
        transfer_data(&mut server_read, &mut client_write);
    });

    client_to_server.join().ok();
    server_to_client.join().ok();

    Ok(())
}

fn perform_websocket_handshake(client_stream: &mut TcpStream, request: &str) -> Result<(), Error> {
    // Extrair chave Sec-WebSocket-Key do cabeçalho HTTP
    let key = extract_websocket_key(request).ok_or_else(|| {
        Error::new(std::io::ErrorKind::InvalidData, "Chave WebSocket inválida")
    })?;

    // Gerar chave de resposta WebSocket
    let accept_key = generate_websocket_accept_key(&key);

    // Enviar resposta de handshake
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
        Upgrade: websocket\r\n\
        Connection: Upgrade\r\n\
        Sec-WebSocket-Accept: {}\r\n\r\n",
        accept_key
    );

    client_stream.write_all(response.as_bytes())?;
    Ok(())
}

fn extract_websocket_key(request: &str) -> Option<String> {
    for line in request.lines() {
        if line.starts_with("Sec-WebSocket-Key:") {
            return line.split(":").nth(1).map(|s| s.trim().to_string());
        }
    }
    None
}

fn generate_websocket_accept_key(key: &str) -> String {
    use sha1::{Digest, Sha1};
    use base64::encode;

    let concatenated = format!("{}{}", key, "258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    let mut hasher = Sha1::new();
    hasher.update(concatenated.as_bytes());
    encode(hasher.finalize())
}

fn transfer_data(read_stream: &mut TcpStream, write_stream: &mut TcpStream) {
    let mut buffer = [0; MAX_BUFFER_SIZE];
    loop {
        match read_stream.read(&mut buffer) {
            Ok(0) => {
                // Conexão encerrada pelo cliente
                eprintln!("Conexão encerrada pelo cliente.");
                break;
            }
            Ok(n) => {
                if n > MAX_BUFFER_SIZE {
                    eprintln!("Requisição excede o tamanho máximo permitido.");
                    break;
                }
                if let Err(e) = write_stream.write_all(&buffer[..n]) {
                    eprintln!("Erro de escrita: {}. Encerrando conexão.", e);
                    break;
                }
            }
            Err(e) => {
                eprintln!("Erro de leitura: {}. Encerrando conexão.", e);
                break;
            }
        }
    }

    // Fechar streams ao final
    read_stream.shutdown(Shutdown::Read).ok();
    write_stream.shutdown(Shutdown::Write).ok();
}

fn peek_stream(read_stream: &TcpStream) -> Result<String, Error> {
    let mut peek_buffer = vec![0; 1024];
    let bytes_peeked = read_stream.peek(&mut peek_buffer)?;
    let data = &peek_buffer[..bytes_peeked];
    let data_str = String::from_utf8_lossy(data);
    Ok(data_str.to_string())
}

fn determine_proxy(client_stream: &mut TcpStream) -> Result<String, Error> {
    if let Ok(data_str) = peek_stream(client_stream) {
        // Verificar se o tráfego corresponde a padrões específicos
        if is_ssh_traffic(&data_str) {
            return Ok(get_ssh_address());
        } else if is_openvpn_traffic(&data_str) {
            return Ok(get_openvpn_address());
        } else {
            eprintln!("Tráfego não identificado. Conectando ao proxy OpenVPN por padrão.");
        }
    } else {
        eprintln!("Erro ao tentar ler dados do cliente. Conectando ao OpenVPN por padrão.");
    }

    Ok(get_openvpn_address())
}

fn is_ssh_traffic(data: &str) -> bool {
    // Verifica se os primeiros bytes indicam tráfego SSH (por exemplo, "SSH-2.0")
    data.starts_with("SSH-")
}

fn is_openvpn_traffic(data: &str) -> bool {
    // Verifica se o tráfego contém padrões comuns do OpenVPN
    // Exemplo: OpenVPN usa protocolos UDP/TLS com cabeçalhos conhecidos
    data.contains("OpenVPN") || data.contains("P_CONTROL_HARD_RESET_CLIENT_V2")
}

fn attempt_connection_with_backoff(addr_proxy: &str) -> Result<TcpStream, Error> {
    let mut retries = 0;
    let max_retries = 5;
    let mut delay = Duration::from_secs(1);

    loop {
        match TcpStream::connect(addr_proxy) {
            Ok(stream) => return Ok(stream),
            Err(e) if retries < max_retries => {
                eprintln!("Erro ao conectar ao proxy {}. Tentando novamente em {} segundos...", addr_proxy, delay.as_secs());
                thread::sleep(delay);
                retries += 1;
                delay *= 2;
            }
            Err(e) => {
                eprintln!("Falha ao conectar ao proxy {} após {} tentativas: {}", addr_proxy, retries, e);
                return Err(e);
            }
        }
    }
}

fn get_port() -> u16 {
    let args: Vec<String> = env::args().collect();
    let mut port = 80;
    for i in 1..args.len() {
        if args[i] == "--port" {
            if i + 1 < args.len() {
                port = args[i + 1].parse().unwrap_or(80);
            }
        }
    }
    port
}

fn get_status() -> String {
    let args: Vec<String> = env::args().collect();
    let mut status = String::from("@RustyManager");
    for i in 1..args.len() {
        if args[i] == "--status" {
            if i + 1 < args.len() {
                status = args[i + 1].clone();
            }
        }
    }
    status
}

fn get_ssh_address() -> String {
    env::var("SSH_PROXY_ADDR").unwrap_or_else(|_| String::from("0.0.0.0:22"))
}

fn get_openvpn_address() -> String {
    env::var("OPENVPN_PROXY_ADDR").unwrap_or_else(|_| String::from("0.0.0.0:1194"))
}
