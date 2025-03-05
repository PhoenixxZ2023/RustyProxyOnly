use std::io::{Erro, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::time::Duration;
use std::{env, thread};

fn principal() {
    let porta = obter_porta();
    let servidor = TcpListener::bind(format!("0.0.0.0:{}", porta)).unwrap();
    iniciar_proxy(servidor);
}

fn iniciar_proxy(servidor: TcpListener) {
    for conexao in servidor.incoming() {
        match conexao {
            Ok(fluxo_cliente) => {
                thread::spawn(move || {
                    manipula_cliente(fluxo_cliente);
                });
            }
            Err(e) => {
                eprintln!("Erro ao aceitar conexão: {}", e);
            }
        }
    }
}

fn manipula_cliente(mut fluxo_cliente: TcpStream) {
    let mut endereco_backend = obter_backend_padrao(); // Backend padrão

    // Inspeciona dados iniciais para detectar protocolo
    match inspecionar_stream(&fluxo_cliente) {
        Ok(dados) => {
            if dados.starts_with("HTTP") {
                // Verifica se é WebSocket
                if dados.contains("Upgrade: websocket") {
                    if let Ok(backend_ws) = obter_backend_websocket() {
                        endereco_backend = backend_ws;
                        if let Err(e) = realizar_handshake_websocket(&mut fluxo_cliente) {
                            eprintln!("Falha no handshake WebSocket: {}", e);
                            return;
                        }
                    }
                } else {
                    // HTTP normal
                    if let Err(e) = enviar_resposta_http(&mut fluxo_cliente) {
                        eprintln!("Falha ao enviar resposta HTTP: {}", e);
                        return;
                    }
                    endereco_backend = obter_backend_http();
                }
            } else if dados.starts_with("SSH-") { // Formato oficial do protocolo SSH
                endereco_backend = obter_backend_ssh();
            } else {
                endereco_backend = obter_backend_openvpn();
            }
        }
        Err(e) => {
            eprintln!("Erro na inspeção inicial: {}", e);
            return;
        }
    }

    // Conecta ao backend apropriado
    let mut fluxo_servidor = match TcpStream::connect(&endereco_backend) {
        Ok(fluxo) => fluxo,
        Err(e) => {
            eprintln!("Falha ao conectar em {}: {}", endereco_backend, e);
            return;
        }
    };

    // Divide os fluxos para proxy bidirecional
    let (mut cliente_leitura, mut cliente_escrita) = (fluxo_cliente.try_clone().unwrap(), fluxo_cliente);
    let (mut servidor_leitura, mut servidor_escrita) = (fluxo_servidor.try_clone().unwrap(), fluxo_servidor);

    let cliente_para_servidor = thread::spawn(move || {
        transfere_dados(&mut cliente_leitura, &mut servidor_escrita);
    });

    let servidor_para_cliente = thread::spawn(move || {
        transfere_dados(&mut servidor_leitura, &mut cliente_escrita);
    });

    let _ = cliente_para_servidor.join();
    let _ = servidor_para_cliente.join();
}

fn transfere_dados(fluxo_entrada: &mut TcpStream, fluxo_saida: &mut TcpStream) {
    let mut buffer = [0; 2048];
    loop {
        match fluxo_entrada.read(&mut buffer) {
            Ok(0) => break, // Conexão fechada
            Ok(n) => {
                if let Err(e) = fluxo_saida.write_all(&buffer[..n]) {
                    eprintln!("Erro ao escrever dados: {}", e);
                    break;
                }
            }
            Err(e) => {
                eprintln!("Erro ao ler dados: {}", e);
                break;
            }
        }
    }
    let _ = fluxo_saida.shutdown(Shutdown::Both);
}

fn inspecionar_stream(fluxo: &TcpStream) -> Result<String, Erro> {
    let mut buffer = [0; 1024];
    let bytes_lidos = fluxo.peek(&mut buffer)?;
    Ok(String::from_utf8_lossy(&buffer[..bytes_lidos]).to_string())
}

fn realizar_handshake_websocket(fluxo: &mut TcpStream) -> Result<(), Erro> {
    // Handshake WebSocket completo
    let resposta = "HTTP/1.1 101 Switching Protocols\r\n\
                   Upgrade: websocket\r\n\
                   Connection: Upgrade\r\n\
                   Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n";
    fluxo.write_all(resposta.as_bytes())
}

fn enviar_resposta_http(fluxo: &mut TcpStream) -> Result<(), Erro> {
    let status = obter_status();
    let resposta = format!("HTTP/1.1 200 {}\r\nContent-Length: 0\r\n\r\n", status);
    fluxo.write_all(resposta.as_bytes())
}

// Funções para obter configurações
fn obter_backend_padrao() -> String {
    obter_valor_arg("--backend").unwrap_or_else(|| "0.0.0.0:22".to_string())
}

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
    let args: Vec<String> = env::args().collect();
    args.iter()
        .position(|a| a == arg)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn obter_porta() -> u16 {
    obter_valor_arg("--port")
        .and_then(|p| p.parse().ok())
        .unwrap_or(80)
}

fn obter_status() -> String {
    obter_valor_arg("--status").unwrap_or_else(|| "@RustyManager".to_string())
}
