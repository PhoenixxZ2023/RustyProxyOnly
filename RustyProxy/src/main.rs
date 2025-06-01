use std::env;
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::{timeout, Duration};

// Estrutura para gerenciar configurações
#[derive(Debug, Clone)] // Adicionado Clone para facilitar
struct Config {
    port: u16,
    status: String,
    ssh_target_addr: String,    // Modificado para String completa (IP:Porta)
    openvpn_target_addr: String, // Modificado para String completa
    http_target_addr: String,   // Novo para HTTP, String completa
    peek_timeout_secs: u64,    // Timeout para peek_stream
    client_handling_timeout_secs: u64, // Timeout para todo o handle_client
}

impl Config {
    fn from_args() -> Self {
        let args: Vec<String> = env::args().collect();

        let mut port = 80;
        let mut status = String::from("@RustyManager");
        let mut ssh_target_addr = String::from("0.0.0.0:22");
        let mut openvpn_target_addr = String::from("0.0.0.0:1194");
        let mut http_target_addr = String::from("0.0.0.0:8080"); // Padrão para HTTP
        let mut peek_timeout_secs = 2;
        let mut client_handling_timeout_secs = 30;

        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--port" => {
                    if i + 1 < args.len() {
                        port = args[i + 1].parse().unwrap_or(port);
                        i += 1;
                    }
                }
                "--status" => {
                    if i + 1 < args.len() {
                        status = args[i + 1].clone();
                        i += 1;
                    }
                }
                "--ssh-target" => { // Ex: --ssh-target 127.0.0.1:22
                    if i + 1 < args.len() {
                        ssh_target_addr = args[i + 1].clone();
                        i += 1;
                    }
                }
                "--ovpn-target" => { // Ex: --ovpn-target 127.0.0.1:1194
                    if i + 1 < args.len() {
                        openvpn_target_addr = args[i + 1].clone();
                        i += 1;
                    }
                }
                "--http-target" => { // Ex: --http-target 127.0.0.1:8080
                    if i + 1 < args.len() {
                        http_target_addr = args[i + 1].clone();
                        i += 1;
                    }
                }
                "--peek-timeout" => {
                    if i + 1 < args.len() {
                        peek_timeout_secs = args[i + 1].parse().unwrap_or(peek_timeout_secs);
                        i += 1;
                    }
                }
                "--client-timeout" => {
                    if i + 1 < args.len() {
                        client_handling_timeout_secs = args[i+1].parse().unwrap_or(client_handling_timeout_secs);
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }

        Config {
            port,
            status,
            ssh_target_addr,
            openvpn_target_addr,
            http_target_addr,
            peek_timeout_secs,
            client_handling_timeout_secs,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    // Iniciando o proxy com configurações carregadas uma vez
    let config = Arc::new(Config::from_args());
    println!("Configurações carregadas: {:?}", config);

    let listener = TcpListener::bind(format!("[::]:{}", config.port)).await?;
    println!("Iniciando serviço na porta: {}", config.port);
    start_http(listener, config).await; // Passa a config para start_http
    Ok(())
}

async fn start_http(listener: TcpListener, config: Arc<Config>) { // Aceita config
    // Adiciona máximo de conexões simultâneas
    let max_connections = Arc::new(Semaphore::new(1000));

    loop {
        // Adquire uma permissão do semáforo. Fazemos o clone do Arc do semáforo.
        let permit = match max_connections.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => {
                // Isso aconteceria se o semáforo fosse fechado, o que não deve ocorrer neste loop.
                println!("Erro ao adquirir permissão do semáforo. Encerrando.");
                return;
            }
        };
        match listener.accept().await {
            Ok((client_stream, addr)) => {
                // Clona o Arc<Config> para mover para a nova task
                let config_clone = config.clone();
                tokio::spawn(async move {
                    let _permit = permit; // Mantém o permit ativo durante a vida da task
                    if let Err(e) = handle_client(client_stream, config_clone).await {
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

async fn handle_client(mut client_stream: TcpStream, config: Arc<Config>) -> Result<(), Error> {
    // Adiciona timeout para manipulação completa do cliente usando o valor da config
    let result = timeout(Duration::from_secs(config.client_handling_timeout_secs), async {
        client_stream
            .write_all(format!("HTTP/1.1 101 {}\r\n\r\n", config.status).as_bytes())
            .await?;

        let mut buffer = vec![0; 2048]; // Buffer para a leitura inicial do cliente
        client_stream.read(&mut buffer).await?; // Lê dados do cliente (ex: após um CONNECT)

        client_stream
            .write_all(format!("HTTP/1.1 200 {}\r\n\r\n", config.status).as_bytes())
            .await?;

        // Determina o endereço do proxy de destino
        let addr_proxy = match detect_protocol(&client_stream, config.clone()).await? {
            "ssh" => config.ssh_target_addr.clone(),
            "openvpn" => config.openvpn_target_addr.clone(),
            "http" => config.http_target_addr.clone(), // Novo roteamento para HTTP
            _ => config.ssh_target_addr.clone(), // Padrão para SSH
        };
        
        println!("Redirecionando para: {}", addr_proxy);

        // Conecta ao servidor de destino. O erro é propagado pelo '?'
        let server_stream = TcpStream::connect(&addr_proxy).await?;
        // Se a linha acima falhar, a função retorna Err(...) e não continua.

        let (client_read, client_write) = client_stream.into_split();
        let (server_read, server_write) = server_stream.into_split();

        // Uso de Arc<Mutex<...>> para as metades das streams
        let client_read = Arc::new(Mutex::new(client_read));
        let client_write = Arc::new(Mutex::new(client_write));
        let server_read = Arc::new(Mutex::new(server_read));
        let server_write = Arc::new(Mutex::new(server_write));

        let client_to_server = transfer_data(client_read, server_write);
        let server_to_client = transfer_data(server_read, client_write);

        tokio::try_join!(client_to_server, server_to_client)?;
        Ok(())
    }).await;

    // Tratamento do resultado do timeout
    match result {
        Ok(Ok(())) => Ok(()), // Operação interna completou com sucesso
        Ok(Err(e)) => { // Operação interna falhou
            // Não precisamos de um println! aqui, pois o erro será logado pela task em start_http
            Err(e) 
        }
        Err(_elapsed_error) => { // Timeout ocorreu
            println!("Timeout de {}s na manipulação do cliente", config.client_handling_timeout_secs);
            Err(Error::new(ErrorKind::TimedOut, "Timeout na manipulação do cliente"))
        }
    }
}

async fn transfer_data(
    read_stream: Arc<Mutex<tokio::net::tcp::OwnedReadHalf>>,
    write_stream: Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
) -> Result<(), Error> {
    let mut buffer = [0; 32768]; // Buffer de 8KB para transferência
    loop {
        let bytes_read = {
            let mut read_guard = read_stream.lock().await;
            read_guard.read(&mut buffer).await?
        };

        if bytes_read == 0 { // Conexão fechada pelo peer
            break;
        }

        let mut write_guard = write_stream.lock().await;
        write_guard.write_all(&buffer[..bytes_read]).await?;
    }
    Ok(())
}

// Função auxiliar para espiar o stream (peek)
async fn peek_stream(stream: &TcpStream) -> Result<String, Error> {
    let mut peek_buffer = vec![0; 32768]; // Buffer para peek
    let bytes_peeked = stream.peek(&mut peek_buffer).await?;
    let data = &peek_buffer[..bytes_peeked];
    Ok(String::from_utf8_lossy(data).to_string())
}

// Função para detectar o protocolo baseado nos dados iniciais
async fn detect_protocol(stream: &TcpStream, config: Arc<Config>) -> Result<&'static str, Error> { // Aceita config
    // Tenta espiar o stream com timeout configurado
    let data = timeout(Duration::from_secs(config.peek_timeout_secs), peek_stream(stream))
        .await
        .map_err(|e| Error::new(ErrorKind::TimedOut, format!("Timeout no peek_stream: {}", e)))? // Mapeia erro de timeout do peek
        .map_err(|e| Error::new(ErrorKind::Other, format!("Erro no peek_stream: {}", e)))?; // Mapeia erro interno do peek_stream

    if data.to_uppercase().contains("SSH") { // Torna a detecção de SSH case-insensitive
        Ok("ssh")
    } else if data.to_uppercase().contains("HTTP") { // Torna a detecção de HTTP case-insensitive
        Ok("http")
    } else if data.is_empty() { // Se nada for detectado (após o handshake inicial do proxy)
        Ok("ssh") // Padrão para SSH se vazio (pode ser um cliente SSH que não enviou dados rapidamente)
    } else {
        // Se não for SSH, HTTP, nem vazio, assume OpenVPN ou outro protocolo configurado para essa rota
        Ok("openvpn") 
    }
}
