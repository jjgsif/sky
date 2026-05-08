//! Diagnostic probe: connect to a running worker socket and send a PING.

use sky_worker::transport::SkyListener;
use std::path::PathBuf;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("sky_worker=debug")
        .init();

    let socket_path = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "/tmp/sky/workers/sky-worker-1.sock".to_string()),
    );

    println!("binding probe socket at {:?}", socket_path);
    let listener = SkyListener::bind(&socket_path)?;

    println!("waiting for worker to connect…");
    let socket = tokio::time::timeout(Duration::from_secs(10), listener.accept()).await??;
    println!("worker connected — sending PING");

    socket.ping("probe", Duration::from_secs(5)).await?;
    println!("PONG received — worker is healthy");

    Ok(())
}
