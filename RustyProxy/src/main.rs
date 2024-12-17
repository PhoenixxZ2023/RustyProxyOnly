use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{self, BufReader, Read, Write, ErrorKind};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls_pemfile::{certs, private_key};
use tokio::net::{TcpListener as TokioListener, TcpStream as TokioStream};
use tokio_rustls::{rustls, TlsAcceptor, server::TlsStream};

const MAX_BUFFER_SIZE: usize = 8192;

// ------------------------------- MAIN ----------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mode = get_mode();
    let port = get_port();

    match mode.as_str() {
        "tls" => {
            println!("Iniciando Proxy TLS na porta {}", port);
            start_tls_proxy(port).await?;
        }
        "tcp" => {
            println!("Iniciando Proxy TCP/HTTP na porta {}", port);
            start_tcp_proxy(port)?;
        }
        _ => {
            eprintln!("Modo desconhecido! Use --mode tls ou --mode tcp");
        }
    }

    Ok(())
}

// --------------------------- TLS PROXY ---------------------------------

async fn start_tls_proxy(port: u16) -> Result<(), Box<dyn Error>> {
    let addr = format!("[::]:{}", port);

    let cert = load_certs(PathBuf::from(get_cert()).as_path())?;
    let key = load_key(PathBuf::from(get_key()).as_path())?;

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert, key)
        .map_err(|err| io::Error::new(ErrorKind::InvalidInput, err))?;

    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TokioListener::bind(&addr).await?;

    loop {
        let (client_socket, _) = listener.accept().await?;
        let acceptor_clone = acceptor.clone();

        tokio::spawn(async move {
            if let Ok(mut tls_stream) = acceptor_clone.accept(client_socket).await {
                let _ = connect_target("127.0.0.1:80", &mut tls_stream).await;
            }
        });
    }
}

async fn connect_target(host: &str, client_socket: &mut TlsStream<TokioStream>) -> Result<(), Box<dyn Error>> {
    let mut target_socket = TokioStream::connect(host).await?;
    do_forwarding(client_socket, &mut target_socket).await?;
    Ok(())
}

async fn do_forwarding(client_socket: &mut TlsStream<TokioStream>, target_socket: &mut TokioStream) -> Result<(), Box<dyn Error>> {
    let (mut client_reader, mut client_writer) = tokio::io::split(client_socket);
    let (mut target_reader, mut target_writer) = tokio::io::split(target_socket);

    tokio::select! {
        _ = tokio::io::copy(&mut client_reader, &mut target_writer) => {}
        _ = tokio::io::copy(&mut target_reader, &mut client_writer) => {}
    }

    Ok(())
}

// ------------------------- TCP/HTTP PROXY ------------------------------

fn start_tcp_proxy(port: u16) -> Result<(), Error> {
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port))?;
    for stream in listener.incoming() {
        if let Ok(mut client_stream) = stream {
            thread::spawn(move || {
                let _ = handle_client(&mut client_stream);
            });
        }
    }
    Ok(())
}

fn handle_client(client_stream: &mut TcpStream) -> Result<(), Error> {
    let addr_proxy = determine_proxy(client_stream)?;
    let mut server_stream = TcpStream::connect(addr_proxy)?;
    
    let (mut client_read, mut client_write) = (client_stream.try_clone()?, client_stream.try_clone()?);
    let (mut server_read, mut server_write) = (server_stream.try_clone()?, server_stream);

    let client_to_server = thread::spawn(move || transfer_data(&mut client_read, &mut server_write));
    let server_to_client = thread::spawn(move || transfer_data(&mut server_read, &mut client_write));

    client_to_server.join().ok();
    server_to_client.join().ok();

    Ok(())
}

fn transfer_data(read_stream: &mut TcpStream, write_stream: &mut TcpStream) {
    let mut buffer = [0; MAX_BUFFER_SIZE];
    loop {
        match read_stream.read(&mut buffer) {
            Ok(0) => break, // Conexão encerrada
            Ok(n) => {
                if write_stream.write_all(&buffer[..n]).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

// ------------------------- UTILITÁRIOS ---------------------------------

fn load_certs(path: &Path) -> io::Result<Vec<CertificateDer<'static>>> {
    certs(&mut BufReader::new(File::open(path)?)).collect()
}

fn load_key(path: &Path) -> io::Result<PrivateKeyDer<'static>> {
    Ok(private_key(&mut BufReader::new(File::open(path)?))
        .unwrap()
        .ok_or_else(|| io::Error::new(ErrorKind::Other, "Chave privada não encontrada"))?)
}

fn determine_proxy(_: &TcpStream) -> Result<String, Error> {
    Ok("127.0.0.1:1194".to_string()) // Exemplo padrão
}

fn get_mode() -> String {
    env::args()
        .nth(2)
        .unwrap_or_else(|| "tls".to_string()) // Default: TLS
}

fn get_port() -> u16 {
    env::args()
        .nth(1)
        .unwrap_or_else(|| "80".to_string())
        .parse()
        .unwrap_or(80)
}

fn get_cert() -> String {
    "/opt/rustymanager/ssl/cert.pem".to_string()
}

fn get_key() -> String {
    "/opt/rustymanager/ssl/key.pem".to_string()
}
