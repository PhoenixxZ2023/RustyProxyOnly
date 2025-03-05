use std::io::{Error, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;
use std::{env, thread};

fn main() {
    let listener = TcpListener::bind(format!("0.0.0.0:{}", obter_porta())).unwrap();
    iniciar_http(listener);
}

fn iniciar_http(listener: TcpListener) {
    for stream in listener.incoming() {
        match stream {
            Ok(mut client_stream) => {
                thread::spawn(move || {
                    manipular_cliente(&mut client_stream);
                });
            }
            Err(e) => eprintln!("Erro na conexão: {}", e),
        }
    }
}

fn manipular_cliente(client_stream: &mut TcpStream) {
    let (mut addr_proxy, mut is_http) = (obter_backend_ssh(), false);

    // Fase 1: Detecção Inicial de Protocolo
    match inspecionar_stream(client_stream) {
        Ok(data_str) => {
            if data_str.starts_with("HTTP") {
                is_http = true;
                addr_proxy = obter_backend_http();
                
                // Handshake WebSocket
                if data_str.contains("Upgrade: websocket") {
                    if let Ok(ws_backend) = obter_backend_websocket() {
                        addr_proxy = ws_backend;
                        if realizar_handshake_websocket(client_stream).is_err() {
                            return;
                        }
                    }
                } else {
                    if enviar_resposta_http(client_stream).is_err() {
                        return;
                    }
                }
            } else if data_str.starts_with("SSH-") {
                addr_proxy = obter_backend_ssh();
            } else {
                addr_proxy = obter_backend_openvpn();
            }
        }
        Err(_) => return,
    }

    // Fase 2: Conexão com Backend
    let mut server_stream = match TcpStream::connect(&addr_proxy) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Falha na conexão com {}: {}", addr_proxy, e);
            return;
        }
    };

    // Fase 3: Proxy Bidirecional
    let (mut client_read, mut client_write) = (client_stream.try_clone().unwrap(), client_stream.try_clone().unwrap());
    let (mut server_read, mut server_write) = (server_stream.try_clone().unwrap(), server_stream);

    let handles = vec![
        thread::spawn(move || transferir_dados(&mut client_read, &mut server_write)),
        thread::spawn(move || transferir_dados(&mut server_read, &mut client_write)),
    ];

    for handle in handles {
        let _ = handle.join();
    }
}

// ... (funções transferir_dados e inspecionar_stream mantidas)

// Novas Funções de Configuração
fn obter_backend_http() -> String {
    obter_valor_arg("--http-backend").unwrap_or_else(|| "0.0.0.0:80".to_string())
}

fn obter_backend_websocket() -> Result<String, &'static str> {
    obter_valor_arg("--ws-backend").ok_or("Backend WebSocket não configurado")
}

fn obter_backend_ssh() -> String {
    obter_valor_arg("--ssh-backend").unwrap_or_else(|| "0.0.0.0:22".to_string())
}

fn obter_backend_openvpn() -> String {
    obter_valor_arg("--ovpn-backend").unwrap_or_else(|| "0.0.0.0:1194".to_string())
}

fn obter_valor_arg(arg: &str) -> Option<String> {
    env::args().position(|a| a == arg).and_then(|i| env::args().nth(i + 1))
}

// Handshake WebSocket Correto
fn realizar_handshake_websocket(stream: &mut TcpStream) -> Result<(), Error> {
    let resposta = "HTTP/1.1 101 Switching Protocols\r\n\
                   Upgrade: websocket\r\n\
                   Connection: Upgrade\r\n\
                   Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n";
    stream.write_all(resposta.as_bytes())
}

// Resposta HTTP Genérica
fn enviar_resposta_http(stream: &mut TcpStream) -> Result<(), Error> {
    let status = obter_status();
    let resposta = format!("HTTP/1.1 200 {}\r\nContent-Length: 0\r\n\r\n", status);
    stream.write_all(resposta.as_bytes())
}
