use anyhow::{anyhow, Result};
use autometrics::{autometrics, prometheus_exporter};
use axum::routing;
use axum::Router;
use clap::Parser;
use rand::{Rng, RngCore};
use tokio::net::TcpListener;
use tonic::transport::Channel;

pub mod grpc {
    tonic::include_proto!("sraft");
}

use grpc::set_response;
use grpc::sraft_client::SraftClient;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    leader: String,
    #[arg(short, long)]
    followers: Vec<String>,

    #[arg(short, long)]
    readers: usize,
    #[arg(short, long)]
    writers: usize,
    #[arg(short, long)]
    payload_size: usize,
}

#[autometrics]
async fn get(client: &mut SraftClient<Channel>) -> Result<Vec<u8>> {
    let req = grpc::GetRequest {
        key: format!("{}", rand::rng().random_range(0..1000)),
    };
    match client.get(req).await.map(|x| x.into_inner()) {
        Ok(v) => Ok(v.value),
        Err(e) => Err(anyhow!("get error: {}", e)),
    }
}

#[autometrics]
async fn set(client: &mut SraftClient<Channel>, payload_size: usize) -> Result<Option<Vec<u8>>> {
    let mut req = grpc::SetRequest {
        key: format!("{}", rand::rng().random_range(0..1000)),
        value: Vec::with_capacity(payload_size),
    };
    rand::rng().fill_bytes(&mut req.value);
    match client.set(req).await.map(|x| x.into_inner()) {
        Ok(v) => Ok(
            if let Some(set_response::PreviousValue::Value(v)) = v.previous_value {
                Some(v)
            } else {
                None
            },
        ),
        Err(e) => Err(anyhow!("set error: {}", e.message())),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let leader = Channel::from_shared(args.leader)?.connect().await?;
    let write_client = SraftClient::new(leader);

    let mut read_clients = Vec::with_capacity(args.followers.len());
    for follower in args.followers {
        let ch = Channel::from_shared(follower)?.connect().await?;
        read_clients.push(SraftClient::new(ch));
    }
    let mut handles = Vec::with_capacity(args.writers + args.readers);
    for _ in 0..args.writers {
        let mut client = write_client.clone();
        handles.push(tokio::spawn(async move {
            loop {
                if let Err(err) = set(&mut client, args.payload_size).await {
                    println!("got set error: {err:#}");
                }
            }
        }));
    }

    for i in 0..args.readers {
        let mut client = read_clients[i % read_clients.len()].clone();
        handles.push(tokio::spawn(async move {
            loop {
                if let Err(err) = get(&mut client).await {
                    println!("got get error: {err:#}");
                }
            }
        }));
    }

    prometheus_exporter::init();
    let app = Router::new().route(
        "/metrics",
        routing::get(|| async { prometheus_exporter::encode_http_response() }),
    );
    let listener = TcpListener::bind("0.0.0.0:8080").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
