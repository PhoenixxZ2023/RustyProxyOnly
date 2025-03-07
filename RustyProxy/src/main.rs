use std::io::{self, Error, ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;
use std::{env, thread};

use threadpool::ThreadPool;

const BUFFER_SIZE: usize = 8192;

fn main() -> io::Result<()> {
    let port = get_port().unwrap_or(80);
    let status = get_status();
    let num_threads = num_cpus::get();
    let pool = ThreadPool::new(num_threads);

    let listener = TcpListener::bind(("0.0.0.0", port))?;
    println!("Proxy listening on port {}", port);

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let status = status.clone();
                pool.execute(move || {
                    if let Err(e) = handle_client(stream, &status) {
                        eprintln!("Erro na conexão: {}", e);
                    }
                });
            }
            Err(e) => eprintln!("Falha na conexão: {}", e),
        }
    }
    Ok(())
}

fn handle_client(mut client: TcpStream, status: &str) -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    let client_clone = client.try_clone()?;

    thread::spawn(move || {
        tx.send(peek_stream(&client_clone)).ok();
    });

    let initial_data = match rx.recv_timeout(Duration::from_secs(2)) {
        Ok(Ok(data)) => data,
        _ => String::new(),
    };

    let target = if initial_data.starts_with("SSH-") {
        "0.0.0.0:22"
    } else if is_http(&initial_data) {
        handle_http(&mut client, status, &initial_data)?;
        "0.0.0.0:80"
    } else {
        "0.0.0.0:1194"
    };

    let mut server = TcpStream::connect(target)?;
    println!("Redirecionando para: {}", target);

    if !initial_data.is_empty() {
        server.write_all(initial_data.as_bytes())?;
    }

    let (mut client_read, mut client_write) = (client.try_clone()?, client);
    let (mut server_read, mut server_write) = (server.try_clone()?, server);

    let client_to_server = thread::spawn(move || {
        transfer(&mut client_read, &mut server_write, "Cliente -> Servidor")
    });

    let server_to_client = thread::spawn(move || {
        transfer(&mut server_read, &mut client_write, "Servidor -> Cliente")
    });

    client_to_server.join().unwrap()?;
    server_to_client.join().unwrap()?;

    Ok(())
}

fn handle_http(client: &mut TcpStream, status: &str, initial_data: &str) -> io::Result<()> {
    if initial_data.starts_with("GET") {
        let response = format!(
            "HTTP/1.1 200 OK\r\n\
            Content-Length: {}\r\n\
            Connection: close\r\n\r\n\
            {}",
            status.len(),
            status
        );

        client.write_all(response.as_bytes())?;
        client.flush()?;
    }
    Ok(())
}

fn is_http(data: &str) -> bool {
    let http_methods = ["GET", "POST", "PUT", "DELETE", "HEAD", "OPTIONS", "PATCH", "TRACE", "CONNECT"];
    http_methods.iter().any(|method| data.starts_with(method)) || data.starts_with("HTTP/")
}

fn transfer(read: &mut TcpStream, write: &mut TcpStream, label: &str) -> io::Result<()> {
    let mut buffer = [0u8; BUFFER_SIZE];
    loop {
        match read.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                write.write_all(&buffer[..n])?;
                write.flush()?;
                println!("{}: {} bytes transferidos", label, n);
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        }
    }
    write.shutdown(Shutdown::Both)?;
    Ok(())
}

fn peek_stream(stream: &TcpStream) -> io::Result<String> {
    let mut buffer = [0u8; 1024];
    let n = stream.peek(&mut buffer)?;
    Ok(String::from_utf8_lossy(&buffer[..n]).into_owned())
}

fn get_port() -> Option<u16> {
    env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "--port")
        .and_then(|w| w[1].parse().ok())
}

fn get_status() -> String {
    env::args()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "--status")
        .map(|w| w[1].clone())
        .unwrap_or_else(|| "Server: RustProxy\r\nX-Status: Online".into())
}
