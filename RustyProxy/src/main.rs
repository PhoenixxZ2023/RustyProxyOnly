use std::env;
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::{timeout, Duration};

// Para o handshake WebSocket
use sha1::{Sha1, Digest};
use base64::Engine; // Importar Engine trait
use base64::engine::general_purpose; // Importar o motor padrão (STANDARD)

// Estrutura para gerenciar configurações
struct Config {
    port: u16,
    status: String,
    ssh_port: u16,
    openvpn_port: u16,
    timeout_secs: u64,
}

impl Config {
    fn from_args() -> Self {
        Config {
            port: get_port(),
            status: get_status(),
            ssh_port: 22,
            openvpn_port: 1194,
            timeout_secs: 1, // Timeout para peek/detecção de protocolo
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let config = Config::from_args();
    let listener = TcpListener::bind(format!("[::]:{}", config.port)).await?;
    println!("Iniciando serviço na porta: {}", config.port);
    start_http(listener).await;
    Ok(())
}

async fn start_http(listener: TcpListener) {
    let max_connections = Arc::new(Semaphore::new(1000));

    loop {
        let permit = max_connections.clone().acquire_owned().await;
        match listener.accept().await {
            Ok((client_stream, addr)) => {
                tokio::spawn(async move {
                    let _permit = permit; // Mantém o permit ativo
                    if let Err(e) = handle_client(client_stream).await {
                        println!("Erro ao processar cliente {}: {}", addr, e);
                    }
                });
            }
            Err(e) => {
                println!("Erro ao aceitar conexão: {}", e);
            }
        }
    }
}

async fn handle_client(mut client_stream: TcpStream) -> Result<(), Error> {
    let config = Config::from_args();
    let result = timeout(Duration::from_secs(30), async {
        // Leia os dados iniciais do cliente para detectar o protocolo
        let mut initial_buffer = vec![0; 4096]; // Buffer maior para ler cabeçalhos HTTP
        let n = client_stream.read(&mut initial_buffer).await?;
        let initial_data_str = String::from_utf8_lossy(&initial_buffer[..n]).to_string();

        let protocol = if initial_data_str.contains("Upgrade: websocket") && initial_data_str.contains("Connection: Upgrade") {
            "websocket"
        } else if initial_data_str.contains("SSH-") { // SSH geralmente começa com "SSH-"
            "ssh"
        } else if initial_data_str.starts_with("GET") || initial_data_str.starts_with("POST") {
            "http"
        } else {
            "openvpn" // Padrão se não for reconhecido, assumindo que openvpn é o default
        };

        println!("Protocolo detectado: {}", protocol);

        match protocol {
            "websocket" => {
                println!("Iniciando Handshake WebSocket.");
                let ws_key_start_tag = "Sec-WebSocket-Key: ";
                let ws_key_end_tag = "\r\n";

                let key_start_idx = initial_data_str.find(ws_key_start_tag)
                    .ok_or_else(|| Error::new(ErrorKind::InvalidData, "Sec-WebSocket-Key não encontrada"))? + ws_key_start_tag.len();
                let key_end_idx = initial_data_str[key_start_idx..].find(ws_key_end_tag)
                    .ok_or_else(|| Error::new(ErrorKind::InvalidData, "Formato de Sec-WebSocket-Key inválido"))? + key_start_idx;
                let client_ws_key = &initial_data_str[key_start_idx..key_end_idx];

                let magic_string = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
                let combined_key = format!("{}{}", client_ws_key, magic_string);

                let mut hasher = Sha1::new();
                hasher.update(combined_key.as_bytes());
                let result_hash = hasher.finalize();
                let encoded_key = general_purpose::STANDARD.encode(result_hash);

                let response = format!(
                    "HTTP/1.1 101 Switching Protocols\r\n\
                     Upgrade: websocket\r\n\
                     Connection: Upgrade\r\n\
                     Sec-WebSocket-Accept: {}\r\n\r\n",
                    encoded_key
                );
                client_stream.write_all(response.as_bytes()).await?;
                println!("Handshake WebSocket concluído.");

                // Conectar ao backend (ex: SSH como um serviço de "tunelamento" WebSocket)
                let addr_proxy = format!("0.0.0.0:{}", config.ssh_port); // Túnel WebSocket para porta SSH
                let server_connect = TcpStream::connect(&addr_proxy).await;
                if server_connect.is_err() {
                    println!("Erro ao iniciar conexão para o proxy {}", addr_proxy);
                    return Err(Error::new(ErrorKind::ConnectionRefused, "Falha ao conectar ao backend"));
                }
                let server_stream = server_connect?;

                websocket_transfer(client_stream, server_stream).await?;
            }
            "http" => {
                // Para requisições HTTP, enviar a resposta de status e então tunelar
                client_stream
                    .write_all(format!("HTTP/1.1 101 {}\r\n\r\n", config.status).as_bytes())
                    .await?;
                // Não precisamos ler mais nada para HTTP se já enviamos o status e vamos tunelar
                // A linha abaixo seria para uma resposta HTTP completa, não para tunelamento
                // client_stream.read(&mut vec![0; 1024]).await?;
                // client_stream
                //     .write_all(format!("HTTP/1.1 200 {}\r\n\r\n", config.status).as_bytes())
                //     .await?;

                let addr_proxy = format!("0.0.0.0:{}", config.ssh_port); // Pode ser 80 ou 443 para HTTP
                let server_connect = TcpStream::connect(&addr_proxy).await;
                if server_connect.is_err() {
                    println!("Erro ao iniciar conexão para o proxy {}", addr_proxy);
                    return Err(Error::new(ErrorKind::ConnectionRefused, "Falha ao conectar ao backend"));
                }
                let server_stream = server_connect?;

                let (client_read, client_write) = client_stream.into_split();
                let (server_read, server_write) = server_stream.into_split();

                let client_to_server = transfer_data(Arc::new(Mutex::new(client_read)), Arc::new(Mutex::new(server_write)));
                let server_to_client = transfer_data(Arc::new(Mutex::new(server_read)), Arc::new(Mutex::new(client_write)));

                tokio::try_join!(client_to_server, server_to_client)?;
            }
            _ => { // SSH, OpenVPN, ou outros
                let addr_proxy = match protocol {
                    "ssh" => format!("0.0.0.0:{}", config.ssh_port),
                    "openvpn" => format!("0.0.0.0:{}", config.openvpn_port),
                    _ => format!("0.0.0.0:{}", config.ssh_port), // Padrão
                };

                let server_connect = TcpStream::connect(&addr_proxy).await;
                if server_connect.is_err() {
                    println!("Erro ao iniciar conexão para o proxy {}", addr_proxy);
                    return Err(Error::new(ErrorKind::ConnectionRefused, "Falha ao conectar ao backend"));
                }
                let server_stream = server_connect?;

                let (client_read, client_write) = client_stream.into_split();
                let (server_read, server_write) = server_stream.into_split();

                let client_to_server = transfer_data(Arc::new(Mutex::new(client_read)), Arc::new(Mutex::new(server_write)));
                let server_to_client = transfer_data(Arc::new(Mutex::new(server_read)), Arc::new(Mutex::new(client_write)));

                tokio::try_join!(client_to_server, server_to_client)?;
            }
        }
        Ok(())
    }).await;

    if let Err(e) = result {
        println!("Timeout na manipulação do cliente: {}", e);
        Err(Error::new(ErrorKind::TimedOut, "Timeout na manipulação do cliente"))
    } else {
        result.unwrap()
    }
}

// Transferência de dados raw (não WebSocket)
async fn transfer_data(
    read_stream: Arc<Mutex<tokio::net::tcp::OwnedReadHalf>>,
    write_stream: Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
) -> Result<(), Error> {
    let mut buffer = [0; 8192];
    loop {
        let bytes_read = {
            let mut read_guard = read_stream.lock().await;
            read_guard.read(&mut buffer).await?
        };

        if bytes_read == 0 {
            break;
        }

        let mut write_guard = write_stream.lock().await;
        write_guard.write_all(&buffer[..bytes_read]).await?;
    }
    Ok(())
}

// Transferência de dados com framing WebSocket
async fn websocket_transfer(
    client_stream: TcpStream,
    server_stream: TcpStream,
) -> Result<(), Error> {
    let (client_read_half, client_write_half) = client_stream.into_split();
    let (server_read_half, server_write_half) = server_stream.into_split();

    // Envolve as metades em Arc<Mutex> para que possam ser compartilhadas
    let client_read = Arc::new(Mutex::new(client_read_half));
    let client_write = Arc::new(Mutex::new(client_write_half));
    let server_read = Arc::new(Mutex::new(server_read_half));
    let server_write = Arc::new(Mutex::new(server_write_half));

    // Task para ler do cliente (WebSocket) e escrever para o servidor (raw)
    let client_to_server_task: tokio::task::JoinHandle<Result<(), Error>> = tokio::spawn(async move {
        let mut buffer = Vec::new(); // Buffer para o payload do WebSocket
        loop {
            // Ler o cabeçalho do frame WebSocket (2 bytes)
            let mut header = [0; 2];
            let bytes_read_header = {
                let mut read_guard = client_read.lock().await;
                read_guard.read_exact(&mut header).await?
            };
            if bytes_read_header == 0 {
                println!("Cliente WebSocket fechou a conexão (leitura de cabeçalho).");
                break Ok::<(), Error>(());
            }

            let fin = (header[0] & 0x80) != 0;
            let opcode = header[0] & 0x0F;
            let masked = (header[1] & 0x80) != 0;
            let mut payload_len = (header[1] & 0x7F) as usize;

            if payload_len == 126 {
                let mut extended_len_bytes = [0; 2];
                let bytes_read_ext_len = {
                    let mut read_guard = client_read.lock().await;
                    read_guard.read_exact(&mut extended_len_bytes).await?
                };
                if bytes_read_ext_len == 0 {
                    break Ok::<(), Error>(());
                }
                payload_len = u16::from_be_bytes(extended_len_bytes) as usize;
            } else if payload_len == 127 {
                let mut extended_len_bytes = [0; 8];
                let bytes_read_ext_len = {
                    let mut read_guard = client_read.lock().await;
                    read_guard.read_exact(&mut extended_len_bytes).await?
                };
                if bytes_read_ext_len == 0 {
                    break Ok::<(), Error>(());
                }
                payload_len = u64::from_be_bytes(extended_len_bytes) as usize;
            }

            let mut masking_key = [0; 4];
            if masked {
                let bytes_read_mask = {
                    let mut read_guard = client_read.lock().await;
                    read_guard.read_exact(&mut masking_key).await?
                };
                if bytes_read_mask == 0 {
                    break Ok::<(), Error>(());
                }
            }

            buffer.resize(payload_len, 0); // Redimensiona o buffer para o tamanho do payload
            let bytes_read_payload = {
                let mut read_guard = client_read.lock().await;
                read_guard.read_exact(&mut buffer).await?
            };
            if bytes_read_payload == 0 {
                break Ok::<(), Error>(());
            }

            // Desmascarar payload
            if masked {
                for i in 0..payload_len {
                    buffer[i] ^= masking_key[i % 4];
                }
            }

            match opcode {
                0x1 | 0x2 => { // Text (0x1) ou Binary (0x2) frames - tunelar
                    let mut write_guard = server_write.lock().await;
                    write_guard.write_all(&buffer[..bytes_read_payload]).await?;
                },
                0x8 => { // Close frame (FIN, Close)
                    println!("Cliente WebSocket pediu para fechar a conexão.");
                    // Enviar um close frame de volta (opcional)
                    let close_frame = [0x88, 0x00];
                    let mut write_guard = client_write.lock().await;
                    write_guard.write_all(&close_frame).await?;
                    let mut server_write_guard = server_write.lock().await;
                    server_write_guard.shutdown().await?; // Fechar a conexão com o servidor
                    break Ok::<(), Error>(()); // Saímos da task
                },
                0x9 => { // Ping frame (FIN, Ping)
                    // Responder com Pong frame
                    let mut pong_frame = Vec::new();
                    pong_frame.push(0x8A); // FIN | Pong
                    if payload_len <= 125 {
                        pong_frame.push(payload_len as u8);
                    } else if payload_len <= 65535 {
                        pong_frame.push(126);
                        pong_frame.extend_from_slice(&(payload_len as u16).to_be_bytes());
                    } else {
                        pong_frame.push(127);
                        pong_frame.extend_from_slice(&(payload_len as u64).to_be_bytes());
                    }
                    pong_frame.extend_from_slice(&buffer[..bytes_read_payload]);
                    let mut write_guard = client_write.lock().await;
                    write_guard.write_all(&pong_frame).await?;
                },
                0xA => { // Pong frame (FIN, Pong) - ignorar
                    // println!("Received Pong from client.");
                },
                _ => {
                    println!("Opcode WebSocket desconhecido/não suportado: {}", opcode);
                }
            }
        }
    });

    // Task para ler do servidor (raw) e escrever para o cliente (WebSocket)
    let server_to_client_task: tokio::task::JoinHandle<Result<(), Error>> = tokio::spawn(async move {
        let mut buffer = [0; 8192];
        loop {
            let bytes_read = {
                let mut read_guard = server_read.lock().await;
                read_guard.read(&mut buffer).await?
            };
            if bytes_read == 0 {
                println!("Servidor backend fechou a conexão.");
                // Enviar close frame ao cliente WebSocket
                let close_frame = [0x88, 0x00]; // FIN | Close, Payload Len = 0
                let mut write_guard = client_write.lock().await;
                write_guard.write_all(&close_frame).await?;
                break Ok::<(), Error>(());
            }

            // Criar frame WebSocket (FIN=1, Opcode=Binary, Mask=0, Payload Len)
            let mut frame = Vec::new();
            frame.push(0x82); // FIN bit set, Opcode 0x2 (Binary Frame)

            if bytes_read <= 125 {
                frame.push(bytes_read as u8);
            } else if bytes_read <= 65535 {
                frame.push(126); // Extended payload length (2 bytes)
                frame.extend_from_slice(&(bytes_read as u16).to_be_bytes());
            } else {
                frame.push(127); // Extended payload length (8 bytes)
                frame.extend_from_slice(&(bytes_read as u64).to_be_bytes());
            }
            frame.extend_from_slice(&buffer[..bytes_read]);

            let mut write_guard = client_write.lock().await;
            write_guard.write_all(&frame).await?;
        }
    });

    tokio::try_join!(client_to_server_task, server_to_client_task)?;
    Ok(())
}

// Esta função não é mais usada para detecção de protocolo, mas é mantida por completude
// O `peek_stream` é apenas para ver os primeiros bytes sem consumi-los
async fn peek_stream(stream: &TcpStream) -> Result<String, Error> {
    let mut peek_buffer = vec![0; 8192];
    let bytes_peeked = stream.peek(&mut peek_buffer).await?;
    let data = &peek_buffer[..bytes_peeked];
    let data_str = String::from_utf8_lossy(data);
    Ok(data_str.to_string())
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
