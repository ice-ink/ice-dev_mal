use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // 0.0.0.0 so remote hosts can connect
    let listener = TcpListener::bind("0.0.0.0:7878").await?;
    println!("TCP server listening on 0.0.0.0:7878");

    loop {
        let (socket, peer) = listener.accept().await?;
        println!("client connected: {peer}");

        tokio::spawn(async move {
            let (reader, mut writer) = socket.into_split();
            let mut lines = BufReader::new(reader).lines();

            while let Ok(Some(line)) = lines.next_line().await {
                println!("[{peer}] {line}");
                // Echo back with newline framing
                if writer.write_all(format!("echo: {line}\n").as_bytes()).await.is_err() {
                    break;
                }
            }
            println!("client disconnected: {peer}");
        });
    }
}