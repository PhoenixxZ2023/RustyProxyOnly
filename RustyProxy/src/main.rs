use std::io::{Error, ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;
use std::{env, thread};

// Constantes configuráveis atualizadas
const PORTA_PADRAO: u16 = 8080;
const TAMANHO_BUFFER: usize = 8192;    // Buffer principal de 8KB
const TAMANHO_PEEK: usize = 1024;      // 8192 / 8 = 1024
const TIMEOUT_CONEXAO: u64 = 10;

fn main() {
    let porta = obter_porta_argumento();
    let status = obter_status_argumento();
    
    println!("🦀 Iniciando proxy Rust na porta: {}", porta);
    println!("🔧 Configuração - Status: '{}'", status);
    
    match TcpListener::bind(format!("0.0.0.0:{}", porta)) {
        Ok(listener) => iniciar_servidor(listener, status),
        Err(e) => eprintln!("Erro ao iniciar servidor: {}", e),
    }
}

fn iniciar_servidor(listener: TcpListener, status: String) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream_cliente) => {
                let status_clone = status.clone();
                thread::spawn(move || {
                    if let Err(e) = lidar_conexao_cliente(stream_cliente, &status_clone) {
                        eprintln!("Erro na conexão: {}", e);
                    }
                });
            }
            Err(e) => eprintln!("Erro na conexão: {}", e),
        }
    }
}

fn lidar_conexao_cliente(mut stream_cliente: TcpStream, status: &str) -> Result<(), Error> {
    let (addr_servidor, eh_http) = detectar_protocolo(&stream_cliente)?;
    
    println!("🔌 Nova conexão de: {}", stream_cliente.peer_addr()?);
    println!("🎯 Conectando ao servidor: {}", addr_servidor);

    let mut stream_servidor = TcpStream::connect(addr_servidor)
        .map_err(|e| Error::new(ErrorKind::Other, format!("Falha ao conectar ao servidor: {}", e)))?;

    stream_cliente.set_read_timeout(Some(Duration::from_secs(TIMEOUT_CONEXAO)))?;
    stream_servidor.set_read_timeout(Some(Duration::from_secs(TIMEOUT_CONEXAO)))?;

    let (mut cliente_leitura, mut cliente_escrita) = (stream_cliente.try_clone()?, stream_cliente.try_clone()?);
    let (mut servidor_leitura, mut servidor_escrita) = (stream_servidor.try_clone()?, stream_servidor);

    let cliente_para_servidor = thread::spawn(move || {
        transferir_dados(
            &mut cliente_leitura,
            &mut servidor_escrita,
            eh_http,
            status,
        )
    });

    let servidor_para_cliente = thread::spawn(move || {
        transferir_dados(
            &mut servidor_leitura,
            &mut cliente_escrita,
            eh_http,
            status,
        )
    });

    let _ = cliente_para_servidor.join();
    let _ = servidor_para_cliente.join();

    Ok(())
}

fn detectar_protocolo(stream: &TcpStream) -> Result<(String, bool), Error> {
    let dados_iniciais = inspecionar_stream(stream)?;
    let mut endereco_servidor = String::new();

    let eh_http = if dados_iniciais.starts_with("SSH-") {
        endereco_servidor = "127.0.0.1:22".to_string();
        false
    } else if dados_iniciais.starts_with("GET ") 
        || dados_iniciais.starts_with("POST ") 
        || dados_iniciais.starts_with("HTTP/")
    {
        endereco_servidor = "127.0.0.1:80".to_string();
        true
    } else {
        endereco_servidor = "127.0.0.1:1194".to_string();
        false
    };

    println!("🔍 Protocolo detectado: {}", match eh_http {
        true => "HTTP",
        false => if endereco_servidor.contains("22") { "SSH" } else { "OpenVPN" }
    });

    Ok((endereco_servidor, eh_http))
}

fn transferir_dados(
    origem: &mut TcpStream,
    destino: &mut TcpStream,
    eh_http: bool,
    status: &str,
) -> Result<(), Error> {
    let mut buffer = [0; TAMANHO_BUFFER]; // Buffer principal de 8KB
    
    loop {
        match origem.read(&mut buffer) {
            Ok(0) => break,
            Ok(bytes_lidos) => {
                let dados_modificados = if eh_http {
                    modificar_resposta_http(&buffer[..bytes_lidos], status)
                } else {
                    buffer[..bytes_lidos].to_vec()
                };
                
                destino.write_all(&dados_modificados)?;
                destino.flush()?;
                
                println!("📤 Transferidos {} bytes", bytes_lidos);
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        }
    }
    
    destino.shutdown(Shutdown::Both)?;
    Ok(())
}

fn modificar_resposta_http(dados: &[u8], status: &str) -> Vec<u8> {
    if let Ok(mut resposta) = String::from_utf8(dados.to_vec()) {
        if resposta.starts_with("HTTP/1.1 200") {
            resposta = resposta.replace("200 OK", &format!("200 {}", status));
        }
        resposta.into_bytes()
    } else {
        dados.to_vec()
    }
}

fn inspecionar_stream(stream: &TcpStream) -> Result<String, Error> {
    let mut buffer = [0; TAMANHO_PEEK]; // Buffer de peek de 1KB
    let bytes_lidos = stream.peek(&mut buffer)?;
    
    String::from_utf8(buffer[..bytes_lidos].to_vec())
        .map_err(|_| Error::new(ErrorKind::InvalidData, "Dados não são UTF-8 válido"))
}

fn obter_porta_argumento() -> u16 {
    env::args()
        .collect::<Vec<String>>()
        .windows(2)
        .find(|args| args[0] == "--port")
        .and_then(|args| args[1].parse().ok())
        .unwrap_or(PORTA_PADRAO)
}

fn obter_status_argumento() -> String {
    env::args()
        .collect::<Vec<String>>()
        .windows(2)
        .find(|args| args[0] == "--status")
        .map(|args| args[1].clone())
        .unwrap_or_else(|| "Proxy Rust 🦀".into())
}
