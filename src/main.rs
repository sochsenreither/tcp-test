use std::net::SocketAddr;
use tokio::time::{sleep, Duration};

mod core;
mod message;
mod network;
mod node;

#[tokio::main]
async fn main() {
    let n = 5;
    let runtime = 15;

    // Create n local ip addresses with different ports.
    let addresses = (0..n)
        .map(|x| format!("127.0.0.1:123{}", x).parse::<SocketAddr>().unwrap())
        .collect::<Vec<_>>();

    // Spawn n nodes.
    for i in 0..n {
        let addresses = addresses.clone();
        tokio::spawn(async move {
            node::Node::new(i, addresses).await;
        });
    }

    sleep(Duration::from_secs(runtime)).await;
}
