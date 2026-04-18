use sky_proto::v1::{worker_control_client::WorkerControlClient, HealthRequest};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("sky_worker=trace,tonic=trace,h2=trace,hyper=debug")
        .init();

    let socket = PathBuf::from("/tmp/manual-test.sock");
    println!("connecting to {:?}", socket);

    // let channel = sky_worker::connect_uds_for_probe(socket).await?;
    let channel = tonic::transport::Endpoint::from_static("http://127.0.0.1:50051")
    .connect()
    .await?;
    println!("channel established");

    let mut client = WorkerControlClient::new(channel);
    println!("sending Health RPC");
    let response = client.health(HealthRequest {}).await?;
    println!("response: {:?}", response.into_inner());

    Ok(())
}