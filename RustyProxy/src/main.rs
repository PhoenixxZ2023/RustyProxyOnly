use std::env;
use std::io::Error;
// Removidos os imports de Arc e Mutex, pois não são mais necessários para transfer_data
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::{time::Duration};
use tokio::time::timeout;

#[tokio::main]
async fn main() -> Result<(), Error> {
    // Iniciando o proxy
    let port = get_port();
    let listener = TcpListener::bind(format!("[::]:{}", port)).await?;
    println!("Iniciando serviço na porta: {}", port);
    start_http(listener).await;
    Ok(())
}

async fn start_http(listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((client_stream, addr)) => {
                tokio::spawn(async move {
                    if let Err(e) = handle_client(client_stream).await {
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

async fn handle_client(mut client_stream: TcpStream) -> Result<(), Error> {
    let status = get_status();
    client_stream
        .write_all(format!("HTTP/1.1 101 {}\r\n\r\n", status).as_bytes())
        .await?;

    let mut buffer = vec![0; 32768];
    // O read aqui parece ser para consumir algo do cliente após o 101, mas
    // não é usado para nada significativo depois. Considere se é realmente necessário.
    client_stream.read(&mut buffer).await?;
    client_stream
        .write_all(format!("HTTP/1.1 200 {}\r\n\r\n", status).as_bytes())
        .await?;

    // --- Melhoria 2: Tratamento de Erros Mais Robusto ---
    let peek_result = timeout(Duration::from_secs(1), peek_stream(&mut client_stream)).await;

    let data_to_check = match peek_result {
        Ok(Ok(data)) => data, // Sucesso em peek_stream dentro do timeout
        Ok(Err(e)) => {
            println!("Erro ao espiar o stream: {}", e);
            String::new() // Continua, mas com uma string vazia
        },
        Err(_) => {
            // Timeout ocorreu
            println!("Tempo limite excedido ao espiar o stream.");
            String::new() // Continua, mas com uma string vazia
        }
    };

    let mut addr_proxy = "0.0.0.0:22"; // Valor padrão para SSH

    if data_to_check.contains("SSH") || data_to_check.is_empty() {
        addr_proxy = "0.0.0.0:22";
    } else {
        addr_proxy = "0.0.0.0:1194";
    }

    let server_connect = TcpStream::connect(addr_proxy).await;
    let server_stream = match server_connect {
        Ok(s) => s,
        Err(e) => {
            println!("Erro ao iniciar conexão para o proxy {}: {}", addr_proxy, e);
            return Ok(()); // Retorna Ok(()) para a tarefa não falhar, mas o erro é logado.
        }
    };

    let (client_read, client_write) = client_stream.into_split();
    let (server_read, server_write) = server_stream.into_split();

    // --- Melhoria 1: Metades do Stream Passadas Diretamente (sem Arc<Mutex>) ---
    let client_to_server = transfer_data(client_read, server_write);
    let server_to_client = transfer_data(server_read, client_write);

    tokio::try_join!(client_to_server, server_to_client)?;

    Ok(())
}

// --- Melhoria 1: Assinatura da Função transfer_data Alterada ---
async fn transfer_data(
    mut read_stream: tokio::net::tcp::OwnedReadHalf, // Recebe por valor
    mut write_stream: tokio::net::tcp::OwnedWriteHalf, // Recebe por valor
) -> Result<(), Error> {
    let mut buffer = [0; 32768];
    loop {
        let bytes_read = read_stream.read(&mut buffer).await?;

        if bytes_read == 0 {
            break; // Conexão fechada
        }

        write_stream.write_all(&buffer[..bytes_read]).await?;
    }

    Ok(())
}

async fn peek_stream(stream: &TcpStream) -> Result<String, Error> {
    let mut peek_buffer = vec![0; 2048];
    let bytes_peeked = stream.peek(&mut peek_buffer).await?;
    let data = &peek_buffer[..bytes_peeked];
    let data_str = String::from_utf8_lossy(data);
    Ok(data_str.to_string())
}

// --- Melhoria 3: Funções get_port e get_status Refatoradas ---
fn get_arg_value(arg_name: &str, default_value: &str) -> String {
    let args: Vec<String> = env::args().collect();
    for i in 1..args.len() {
        if args[i] == arg_name {
            if i + 1 < args.len() {
                return args[i + 1].clone();
            }
        }
    }
    default_value.to_string()
}

fn get_port() -> u16 {
    get_arg_value("--port", "80").parse().unwrap_or(80)
}

fn get_status() -> String {
    get_arg_value("--status", "@RustyManager")
}
