use std::io::{Error, ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;
use std::{env, thread};

use clap::{Arg, Command};

fn main() {
    let matches = Command::new("Rusty Proxy")
        .version("1.0")
        .arg(Arg::new("port")
            .short('p')
            .long("port")
            .value_name("PORT")
            .help("Proxy listening port")
            .default_value("8080"))
        .arg(Arg::new("ssh")
            .long("ssh-port")
            .value_name("PORT")
            .help("SSH backend port")
            .default_value("22"))
        .arg(Arg::new("ovpn")
            .long("openvpn-port")
            .value_name("PORT")
            .help("OpenVPN backend port")
            .default_value("1194"))
        .arg(Arg::new("http")
            .long("http-port")
            .value_name("PORT")
            .help("HTTP backend port")
            .default_value("80"))
        .arg(Arg::new("status")
            .short('s')
            .long("status")
            .value_name("TEXT")
            .help("Custom status message")
            .default_value("Rusty Proxy"))
        .get_matches();

    let port = matches.get_one::<String>("port").unwrap();
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).unwrap();
    start_proxy(listener, matches);
}

fn start_proxy(listener: TcpListener, matches: clap::ArgMatches) {
    for stream in listener.incoming() {
        match stream {
            Ok(mut client_stream) => {
                let matches_clone = matches.clone();
                thread::spawn(move || {
                    handle_client(&mut client_stream, &matches_clone);
                });
            }
            Err(e) => eprintln!("Connection failed: {}", e),
        }
    }
}

fn handle_client(client_stream: &mut TcpStream, matches: &clap::ArgMatches) {
    let initial_data = match peek_stream(client_stream) {
        Ok(data) => data,
        Err(_) => return,
    };

    let status = matches.get_one::<String>("status").unwrap();
    let (backend_addr, mut initial_response) = detect_protocol(&initial_data, matches, status);

    let mut server_stream = match TcpStream::connect(&backend_addr) {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("Failed to connect to {}: {}", backend_addr, e);
            return;
        }
    };

    if let Some(response) = initial_response.take() {
        if client_stream.write_all(response.as_bytes()).is_err() {
            return;
        }
    }

    forward_data(client_stream, &mut server_stream, &initial_data);
}

fn detect_protocol(
    data: &str,
    matches: &clap::ArgMatches,
    status: &str,
) -> (String, Option<String>) {
    if data.starts_with("SSH-") {
        (format!("127.0.0.1:{}", matches.get_one::<String>("ssh").unwrap()), None)
    } else if data.starts_with("GET ") || data.starts_with("POST ") || data.starts_with("HEAD ") {
        handle_http_protocol(data, matches, status)
    } else {
        (format!("127.0.0.1:{}", matches.get_one::<String>("ovpn").unwrap()), None)
    }
}

fn handle_http_protocol(
    data: &str,
    matches: &clap::ArgMatches,
    status: &str,
) -> (String, Option<String>) {
    let is_websocket = data.contains("Upgrade: websocket") || data.contains("upgrade: websocket");
    
    if is_websocket {
        (
            format!("127.0.0.1:{}", matches.get_one::<String>("http").unwrap()),
            Some(
                "HTTP/1.1 101 Switching Protocols\r\n\
                Upgrade: websocket\r\n\
                Connection: Upgrade\r\n\r\n".to_string()
            )
        )
    } else {
        (
            format!("127.0.0.1:{}", matches.get_one::<String>("http").unwrap()),
            Some(format!("HTTP/1.1 200 {}\r\n\r\n", status))
        )
    }
}

fn forward_data(client_stream: &mut TcpStream, server_stream: &mut TcpStream, initial_data: &str) {
    let (mut client_read, mut client_write) = match client_stream.try_clone() {
        Ok((read, write)) => (read, write),
        Err(_) => return,
    };

    let (mut server_read, mut server_write) = match server_stream.try_clone() {
        Ok((read, write)) => (read, write),
        Err(_) => return,
    };

    // Write initial peeked data to backend
    if !initial_data.is_empty() {
        if let Err(e) = server_write.write_all(initial_data.as_bytes()) {
            eprintln!("Initial write failed: {}", e);
            return;
        }
    }

    let client_to_server = thread::spawn(move || {
        transfer_data(&mut client_read, &mut server_write);
    });

    let server_to_client = thread::spawn(move || {
        transfer_data(&mut server_read, &mut client_write);
    });

    client_to_server.join().ok();
    server_to_client.join().ok();
}

fn transfer_data(read_stream: &mut TcpStream, write_stream: &mut TcpStream) {
    let mut buffer = [0; 2048];
    loop {
        match read_stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                if write_stream.write_all(&buffer[..n]).is_err() {
                    break;
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => continue,
            Err(_) => break,
        }
    }
    write_stream.shutdown(Shutdown::Both).ok();
}

fn peek_stream(stream: &TcpStream) -> Result<String, Error> {
    let mut buffer = vec![0; 1024];
    let bytes_peeked = stream.peek(&mut buffer)?;
    Ok(String::from_utf8_lossy(&buffer[..bytes_peeked]).to_string())
}
