use std::io::{Error, ErrorKind, Read, Result, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::time::Duration;
use std::{env, thread};

fn main() -> Result<()> {
    let port = get_port();
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port))?;
    println!("🚀 Proxy iniciado na porta: {}", port);
    start_proxy(listener)
}

fn start_proxy(listener: TcpListener) -> Result<()> {
    for stream in listener.incoming() {
        match stream {
            Ok(client_stream) => {
                thread::spawn(move || {
                    if let Err(e) = handle_client(client_stream) {
                        eprintln!("Erro no cliente: {}", e);
                    }
                });
            }
            Err(e) => eprintln!("Falha na conexão: {}", e),
        }
    }
    Ok(())
}

fn handle_client(mut client_stream: TcpStream) -> Result<()> {
    let (ssh_addr, openvpn_addr) = (get_ssh_addr(), get_openvpn_addr());
    let status = get_status();

    // Detecção inicial do protocolo
    let initial_data = match peek_stream(&client_stream, Duration::from_secs(2)) {
        Ok(data) => data,
        Err(_) => return proxy_traffic(client_stream, &openvpn_addr),
    };

    // Detecção SSH
    if initial_data.starts_with("SSH-") {
        println!("🔑 Conexão SSH detectada");
        return proxy_traffic(client_stream, &ssh_addr);
    }

    // Detecção HTTP/WebSocket
    if initial_data.starts_with("GET") || initial_data.starts_with("POST") || initial_data.starts_with("HTTP") {
        let is_websocket = initial_data.contains("websocket") || initial_data.contains("Upgrade:");
        
        // Construir resposta HTTP correta
        let response = if is_websocket {
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n"
        } else {
            &format!("HTTP/1.1 200 {}\r\nContent-Length: 0\r\n\r\n", status)
        };

        if let Err(e) = client_stream.write_all(response.as_bytes()) {
            return Err(Error::new(ErrorKind::Other, format!("Falha ao responder HTTP: {}", e));
        }

        println!("🌐 Conexão {} detectada", if is_websocket { "WebSocket" } else { "HTTP" });
        return proxy_traffic(client_stream, &get_http_backend(is_websocket));
    }

    // Padrão: OpenVPN
    println!("🛡️ Conexão OpenVPN detectada");
    proxy_traffic(client_stream, &openvpn_addr)
}

fn proxy_traffic(mut client: TcpStream, backend_addr: &str) -> Result<()> {
    let mut backend = TcpStream::connect(backend_addr).map_err(|e| {
        Error::new(
            ErrorKind::Other,
            format!("Falha ao conectar no backend {}: {}", backend_addr, e),
        )
    })?;

    let (mut client_reader, mut client_writer) = (client.try_clone()?, client.try_clone()?);
    let (mut backend_reader, mut backend_writer) = (backend.try_clone()?, backend);

    let client_to_backend = thread::spawn(move || {
        transfer_data(&mut client_reader, &mut backend_writer, "client -> backend")
    });

    let backend_to_client = thread::spawn(move || {
        transfer_data(&mut backend_reader, &mut client_writer, "backend -> client")
    });

    client_to_backend.join().unwrap()?;
    backend_to_client.join().unwrap()?;

    Ok(())
}

fn transfer_data(
    read: &mut TcpStream,
    write: &mut TcpStream,
    direction: &str,
) -> Result<()> {
    let mut buf = [0; 4096];
    loop {
        let bytes = match read.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => return Err(Error::new(ErrorKind::Other, format!("Erro na leitura ({}): {}", direction, e))),
        };

        if let Err(e) = write.write_all(&buf[..bytes]) {
            return Err(Error::new(ErrorKind::Other, format!("Erro na escrita ({}): {}", direction, e)));
        }
    }
    write.shutdown(Shutdown::Both)?;
    Ok(())
}

fn peek_stream(stream: &TcpStream, timeout: Duration) -> Result<String> {
    let mut buf = [0; 1024];
    stream.set_read_timeout(Some(timeout))?;
    let bytes = stream.peek(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf[..bytes]).to_string())
}

// Funções de configuração
fn get_ssh_addr() -> String {
    get_arg_value("--ssh").unwrap_or_else(|| "0.0.0.0:22".into())
}

fn get_openvpn_addr() -> String {
    get_arg_value("--openvpn").unwrap_or_else(|| "0.0.0.0:1194".into())
}

fn get_http_backend(websocket: bool) -> String {
    if websocket {
        get_arg_value("--websocket").unwrap_or_else(|| "0.0.0.0:8080".into())
    } else {
        get_arg_value("--http").unwrap_or_else(|| "0.0.0.0:80".into())
    }
}

fn get_port() -> u16 {
    get_arg_value("--port")
        .and_then(|p| p.parse().ok())
        .unwrap_or(80)
}

fn get_status() -> String {
    get_arg_value("--status").unwrap_or_else(|| "Proxy RustyManager".into())
}

fn get_arg_value(flag: &str) -> Option<String> {
    let args: Vec<String> = env::args().collect();
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
