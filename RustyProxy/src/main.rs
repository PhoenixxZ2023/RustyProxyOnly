use std::io::{Error, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::env;
use std::time::Duration;
use std::sync::{Arc, Mutex};
use std::thread;

const MAX_BUFFER_SIZE: usize = 8192;

fn main() {
    let port = get_port();
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).unwrap_or_else(|e| {
        eprintln!("Erro ao iniciar o listener na porta {}: {}", port, e);
        std::process::exit(1);
    });

    println!("Proxy iniciado na porta {}", port);

    let active_connections = Arc::new(Mutex::new(0));
    for stream in listener.incoming() {
        match stream {
            Ok(client_stream) => {
                let active_connections = Arc::clone(&active_connections);
                thread::spawn(move || {
                    {
                        let mut count = active_connections.lock().unwrap();
                        *count += 1;
                        println!("Conexões ativas: {}", count);
                    }

                    if let Err(e) = handle_client(client_stream) {
                        eprintln!("Erro ao processar cliente: {}", e);
                    }

                    {
                        let mut count = active_connections.lock().unwrap();
                        *count -= 1;
                        println!("Conexões ativas: {}", count);
                    }
                });
            }
            Err(e) => {
                eprintln!("Erro ao aceitar conexão: {}", e);
            }
        }
    }
}

fn handle_client(mut client_stream: TcpStream) -> Result<(), Error> {
    // Define timeouts para evitar bloqueios
    client_stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    client_stream.set_write_timeout(Some(Duration::from_secs(60)))?;

    // Leitura inicial para determinar o tipo de tráfego
    let addr_proxy = determine_proxy(&mut client_stream)?;

    // Tentativa de conexão com backoff exponencial
    let mut server_stream = attempt_connection_with_backoff(&addr_proxy)?;

    let (mut client_read, mut client_write) = (client_stream.try_clone()?, client_stream);
    let (mut server_read, mut server_write) = (server_stream.try_clone()?, server_stream);

    let client_to_server = thread::spawn(move || {
        transfer_data(&mut client_read, &mut server_write);
    });

    let server_to_client = thread::spawn(move || {
        transfer_data(&mut server_read, &mut client_write);
    });

    client_to_server.join().unwrap();
    server_to_client.join().unwrap();

    Ok(())
}

fn transfer_data(read_stream: &mut TcpStream, write_stream: &mut TcpStream) {
    let mut buffer = [0; MAX_BUFFER_SIZE];
    loop {
        match read_stream.read(&mut buffer) {
            Ok(0) => break, // Conexão fechada
            Ok(n) => {
                if n > MAX_BUFFER_SIZE {
                    eprintln!("Requisição excede o tamanho máximo permitido.");
                    break;
                }

                if let Err(e) = write_stream.write_all(&buffer[..n]) {
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
}

fn determine_proxy(client_stream: &mut TcpStream) -> Result<String, Error> {
    let mut peek_buffer = vec![0; 1024];
    match client_stream.peek(&mut peek_buffer) {
        Ok(bytes_peeked) => {
            let data = &peek_buffer[..bytes_peeked];
            let data_str = String::from_utf8_lossy(data).to_lowercase();

            if data_str.contains("ssh") {
                Ok(get_ssh_address())
            } else if data_str.contains("openvpn") {
                Ok(get_openvpn_address())
            } else {
                eprintln!("Tráfego desconhecido, conectando ao OpenVPN por padrão.");
                Ok(get_openvpn_address())
            }
        }
        Err(_) => {
            eprintln!("Erro ao determinar tipo de tráfego, conectando ao OpenVPN por padrão.");
            Ok(get_openvpn_address())
        }
    }
}

fn attempt_connection_with_backoff(addr_proxy: &str) -> Result<TcpStream, Error> {
    let mut retries = 0;
    let max_retries = 5;
    let mut delay = Duration::from_secs(1);

    while retries < max_retries {
        match TcpStream::connect(addr_proxy) {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                eprintln!(
                    "Erro ao conectar ao proxy {}: {}. Tentando novamente em {} segundos...",
                    addr_proxy,
                    e,
                    delay.as_secs()
                );
                thread::sleep(delay);
                retries += 1;
                delay *= 2; // Backoff exponencial
            }
        }
    }

    Err(Error::new(
        std::io::ErrorKind::TimedOut,
        format!("Falha ao conectar ao proxy {} após {} tentativas", addr_proxy, retries),
    ))
}

fn get_port() -> u16 {
    let args: Vec<String> = env::args().collect();
    args.iter()
        .position(|arg| arg == "--port")
        .and_then(|pos| args.get(pos + 1))
        .and_then(|port_str| port_str.parse().ok())
        .unwrap_or(80)
}

fn get_ssh_address() -> String {
    env::var("SSH_PROXY_ADDR").unwrap_or_else(|_| String::from("127.0.0.1:22"))
}

fn get_openvpn_address() -> String {
    env::var("OPENVPN_PROXY_ADDR").unwrap_or_else(|_| String::from("127.0.0.1:1194"))
}
