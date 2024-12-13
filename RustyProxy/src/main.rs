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
    client_stream.write_all(format!("HTTP/1.1 101 {}\r\n\r\n", status).as_bytes())?;

    client_stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    client_stream.set_write_timeout(Some(Duration::from_secs(30)))?;

    match peek_stream(client_stream) {
        Ok(data_str) => {
            if data_str.contains("HTTP") {
                let _ = client_stream.read(&mut vec![0; 1024]);
                if is_websocket_request(&data_str) {
                    client_stream.write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes())?;
                }
            }
        }
        Err(e) => return Err(e),
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

fn is_websocket_request(data_str: &str) -> bool {
    data_str.to_lowercase().contains("upgrade: websocket")
}

fn determine_proxy(client_stream: &mut TcpStream) -> Result<String, Error> {
    let addr_proxy = if let Ok(data_str) = peek_stream(client_stream) {
        if is_websocket_request(&data_str) {
            eprintln!("Tráfego WebSocket detectado. Conectando ao proxy WebSocket.");
            get_websocket_address()
        } else if data_str.contains("SSH") {
            get_ssh_address()
        } else if data_str.contains("OpenVPN") {
            get_openvpn_address()
        } else {
            eprintln!("Tipo de tráfego desconhecido, conectando ao proxy OpenVPN por padrão.");
            get_openvpn_address()
        }
    } else {
        eprintln!("Erro ao tentar ler dados do cliente. Conectando ao OpenVPN por padrão.");
        get_openvpn_address()
    };

    Ok(addr_proxy)
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

fn get_websocket_address() -> String {
    env::var("WEBSOCKET_PROXY_ADDR").unwrap_or_else(|_| String::from("0.0.0.0:8080"))
}
