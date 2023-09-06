use chrono::Local;
use clap::{App, Arg};
use std::error::Error;
use std::fs::File;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{split, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::webpki::DNSNameRef;
use tokio_rustls::TlsConnector;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let matches = App::new("Telnet Client")
        .version("1.0")
        .author("Masanori Kusunoki <masanork@gmail.com>")
        .about("tsrfv: telnets 電子公告ビューア")
        .arg(
            Arg::with_name("SERVER")
                .help("Server address")
                .default_value("koukoku.shadan.open.ad.jp"),
        )
        .arg(
            Arg::with_name("PORT")
                .help("Port number")
                .default_value("992"),
        )
        .get_matches();

    let server = matches.value_of("SERVER").unwrap();
    let port = matches.value_of("PORT").unwrap();
    let addr = format!("{}:{}", server, port);

    let stream = TcpStream::connect(&addr).await?;

    // Setting up the TLS connection
    let mut config = ClientConfig::new();
    let mut root_store = RootCertStore::empty();
    root_store.add_server_trust_anchors(&webpki_roots::TLS_SERVER_ROOTS);
    config.root_store = root_store;

    let connector = TlsConnector::from(Arc::new(config));
    let domain = DNSNameRef::try_from_ascii_str(server).unwrap();
    let stream = connector.connect(domain, stream).await?;

    let (mut reader, mut writer) = split(stream);

    let finished_flag = Arc::new(AtomicBool::new(false));
    let flag_for_write = finished_flag.clone();

    let read_task = tokio::spawn(async move {
        let mut buffer = vec![0u8; 4096];
        let mut accumulated_data = Vec::new();

        loop {
            let read_result =
                tokio::time::timeout(std::time::Duration::from_secs(5), reader.read(&mut buffer))
                    .await;

            match read_result {
                Ok(Ok(n)) if n == 0 => break,
                Ok(Ok(n)) => {
                    let decoded = String::from_utf8_lossy(&buffer[..n]);
                    accumulated_data.extend_from_slice(decoded.as_bytes());
                    print!("{}", decoded);
                    io::stdout().flush().unwrap();
                }
                Ok(Err(e)) => {
                    eprintln!("Read error: {}", e);
                }
                Err(_) => {
                    // 5秒間データの受信が途切れた場合
                    eprintln!("No data received for 5 seconds. Disconnecting...");
                    break;
                }
            }
        }

        // バッファの内容をファイルに保存
        let current_time = Local::now();
        let filename = current_time.format("%Y%m%d%H%M.txt").to_string();
        if let Err(e) = File::create(&filename).and_then(|mut f| f.write_all(&accumulated_data)) {
            eprintln!("Failed to write data to {}: {}", filename, e);
        }

        finished_flag.store(true, Ordering::SeqCst);
    });

    let write_task = tokio::spawn(async move {
        let mut input_buffer = String::new();

        while !flag_for_write.load(Ordering::SeqCst) {
            input_buffer.clear();
            std::io::stdin()
                .read_line(&mut input_buffer)
                .expect("Failed to read from stdin");
            let encoded = input_buffer.as_bytes();
            if let Err(e) = writer.write_all(&encoded).await {
                eprintln!("Write error: {}", e);
            }
        }
    });

    tokio::try_join!(read_task, write_task)?;

    // プログラムを終了します。
    std::process::exit(0);
}
