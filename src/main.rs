use std::net::SocketAddr;
use tokio::time::{sleep, Duration};

mod core;
mod message;
mod network;
mod node;

#[tokio::main]
async fn main() {
    let nodes = 8;
    let runtime = 15;

    // Create 4 local ip addresses with different ports.
    let addresses = (0..nodes)
        .map(|x| format!("127.0.0.1:123{}", x).parse::<SocketAddr>().unwrap())
        .collect::<Vec<_>>();

    // Spawn 4 nodes.
    for i in 0..nodes {
        let addresses = addresses.clone();
        tokio::spawn(async move {
            node::Node::new(i, addresses).await;
        });
    }

    // Wait 15 seconds before terminating.
    sleep(Duration::from_secs(runtime)).await;
}
