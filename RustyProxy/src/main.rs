use std::env;
use std::io::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt, copy_bidirectional};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{timeout, Duration};

// Estrutura para armazenar configurações
struct Config {
    listen_port: u16,
    ssh_port: u16,
    openvpn_port: u16,
    http_port: u16,
    websocket_port: u16,
    status: String,
}

impl Config {
    fn from_args() -> Self {
        let args: Vec<String> = env::args().collect();
        let mut config = Config {
            listen_port: 80,         // Porta de escuta padrão (WebSocket e HTTP)
            ssh_port: 22,            // Porta SSH padrão
            openvpn_port: 1194,      // Porta OpenVPN padrão
            http_port: 8080,         // Porta HTTP padrão
            websocket_port: 9000,    // Porta WebSocket padrão
            status: String::from("@RUSTY PROXY"),
        };

        for i in 1..args.len() {
            match args[i].as_str() {
                "--port" => {
                    if i + 1 < args.len() {
                        config.listen_port = args[i + 1].parse().unwrap_or(80);
                    }
                }
                "--ssh-port" => {
                    if i + 1 < args.len() {
                        config.ssh_port = args[i + 1].parse().unwrap_or(22);
                    }
                }
                "--openvpn-port" => {
                    if i + 1 < args.len() {
                        config.openvpn_port = args[i + 1].parse().unwrap_or(1194);
                    }
                }
                "--http-port" => {
                    if i + 1 < args.len() {
                        config.http_port = args[i + 1].parse().unwrap_or(8080);
                    }
                }
                "--websocket-port" => {
                    if i + 1 < args.len() {
                        config.websocket_port = args[i + 1].parse().unwrap_or(9000);
                    }
                }
                "--status" => {
                    if i + 1 < args.len() {
                        config.status = args[i + 1].clone();
                    }
                }
                _ => {}
            }
        }
        config
    }
}

// Implementação manual simplificada de SHA-1
fn sha1_manual(input: &[u8]) -> [u8; 20] {
    let mut h0 = 0x67452301u32;
    let mut h1 = 0xEFCDAB89u32;
    let mut h2 = 0x98BADCFEu32;
    let mut h3 = 0x10325476u32;
    let mut h4 = 0xC3D2E1F0u32;

    let mut padded = Vec::new();
    padded.extend_from_slice(input);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0x00);
    }
    let len_bits = (input.len() as u64) * 8;
    padded.extend_from_slice(&len_bits.to_be_bytes());

    for chunk in padded.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16];
            w[i] = w[i].rotate_left(1);
        }

        let mut a = h0;
        let mut b = h1;
        let mut c = h2;
        let mut d = h3;
        let mut e = h4;

        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                60..=79 => (b ^ c ^ d, 0xCA62C1D6),
                _ => unreachable!(),
            };

            let temp = a.rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    let mut result = [0u8; 20];
    result[0..4].copy_from_slice(&h0.to_be_bytes());
    result[4..8].copy_from_slice(&h1.to_be_bytes());
    result[8..12].copy_from_slice(&h2.to_be_bytes());
    result[12..16].copy_from_slice(&h3.to_be_bytes());
    result[16..20].copy_from_slice(&h4.to_be_bytes());
    result
}

// Implementação manual de Base64
fn base64_manual(input: &[u8]) -> String {
    const BASE64_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = Vec::new();

    for chunk in input.chunks(3) {
        let mut buf = [0; 3];
        buf[..chunk.len()].copy_from_slice(chunk);
        let n = ((buf[0] as u32) << 16) | ((buf[1] as u32) << 8) | (buf[2] as u32);

        result.push(BASE64_CHARS[(n >> 18) as usize] as char);
        result.push(BASE64_CHARS[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(BASE64_CHARS[((n >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(BASE64_CHARS[(n & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }

    result.into_iter().collect()
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let config = Config::from_args();
    let listener = TcpListener::bind(format!("[::]:{}", config.listen_port)).await?;
    println!("Servidor iniciado na porta: {}", config.listen_port);
    start_proxy(listener, config).await;
    Ok(())
}

async fn start_proxy(listener: TcpListener, config: Config) {
    loop {
        match listener.accept().await {
            Ok((client_stream, addr)) => {
                let config = config.clone();
                println!("Nova conexão de: {}", addr);
                tokio::spawn(async move {
                    if let Err(e) = handle_client(client_stream, &config).await {
                        eprintln!("Erro ao processar cliente {}: {}", addr, e);
                    }
                });
            }
            Err(e) => eprintln!("Erro ao aceitar conexão: {}", e),
        }
    }
}

async fn handle_client(mut client_stream: TcpStream, config: &Config) -> Result<(), Error> {
    // Lê a solicitação inicial do cliente
    let mut buffer = [0; 1024];
    client_stream.read(&mut buffer).await?;
    let request = String::from_utf8_lossy(&buffer);

    // Define o endereço de redirecionamento baseado na solicitação inicial
    let addr_proxy = if request.starts_with("GET ") && 
                       request.contains("Upgrade: websocket") && 
                       request.contains("Connection: Upgrade") {
        // WebSocket na porta 80 com handshake completo
        if let Some(key) = request.lines()
            .find(|line| line.starts_with("Sec-WebSocket-Key"))
            .and_then(|line| line.split(": ").nth(1)) {
            
            let accept_key = {
                let input = format!("{}258EAFA5-E914-47DA-95CA-C5AB0DC85B11", key.trim());
                let hash = sha1_manual(input.as_bytes());
                base64_manual(&hash)
            };

            let response = format!(
                "HTTP/1.1 101 Switching Protocols\r\n\
                 Upgrade: websocket\r\n\
                 Connection: Upgrade\r\n\
                 Sec-WebSocket-Accept: {}\r\n\r\n",
                accept_key
            );
            client_stream.write_all(response.as_bytes()).await?;
        } else {
            client_stream
                .write_all(format!("HTTP/1.1 101 {}\r\n\r\n", config.status).as_bytes())
                .await?;
        }
        format!("0.0.0.0:{}", config.websocket_port)
    } else if request.starts_with("GET ") || request.starts_with("POST ") || 
              request.starts_with("HEAD ") || request.starts_with("PUT ") || 
              request.starts_with("DELETE ") {
        // HTTP na porta 80
        client_stream
            .write_all(format!("HTTP/1.1 101 {}\r\n\r\n", config.status).as_bytes())
            .await?;
        client_stream
            .write_all(format!("HTTP/1.1 200 {}\r\n\r\n", config.status).as_bytes())
            .await?;
        format!("0.0.0.0:{}", config.http_port)
    } else if request.contains("SSH") || request.is_empty() {
        // SSH
        client_stream
            .write_all(format!("HTTP/1.1 101 {}\r\n\r\n", config.status).as_bytes())
            .await?;
        format!("0.0.0.0:{}", config.ssh_port)
    } else {
        // OpenVPN como fallback
        client_stream
            .write_all(format!("HTTP/1.1 101 {}\r\n\r\n", config.status).as_bytes())
            .await?;
        format!("0.0.0.0:{}", config.openvpn_port)
    };

    let mut server_stream = match TcpStream::connect(&addr_proxy).await {
        Ok(stream) => stream,
        Err(_) => {
            eprintln!("Erro ao conectar-se ao servidor proxy em {}", addr_proxy);
            return Ok(());
        }
    };

    // Transfere dados entre cliente e servidor com buffer dinâmico
    let _ = copy_bidirectional(&mut client_stream, &mut server_stream).await;

    Ok(())
}

async fn peek_stream(stream: &mut TcpStream) -> Result<String, Error> {
    let mut buffer = vec![0; 8192];
    let bytes_peeked = stream.peek(&mut buffer).await?;
    Ok(String::from_utf8_lossy(&buffer[..bytes_peeked]).to_string())
}
