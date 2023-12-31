use crate::message::NetworkMessage;
use bytes::Bytes;
use futures::{stream::futures_unordered::FuturesUnordered, SinkExt, StreamExt};
use std::{collections::HashMap, net::SocketAddr};
use tokio::sync::oneshot;
use tokio::time::{sleep, Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc::{channel, Receiver, Sender},
};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

#[cfg(test)]
#[path = "tests/network_tests.rs"]
pub mod network_tests;

pub struct NetworkRetransmitter;

impl NetworkRetransmitter {
    pub fn run(mut rx: Receiver<(NetworkMessage, SocketAddr)>, tx: Sender<NetworkMessage>) {
        tokio::spawn(async move {
            let mut pending = FuturesUnordered::new();
            loop {
                tokio::select! {
                    Some((mes, addr)) = rx.recv() => {
                        println!("Incoming message, addr: {}", addr.clone());
                        let new_message = NetworkMessage {
                            sender: mes.sender,
                            addresses: vec![addr],
                            message: mes.message.clone(),
                        };
                        pending.push(Self::delay(new_message));
                    }
                    Some(mes) = pending.next() => tx.send(mes).await.unwrap(),
                }
            }
        });
    }

    async fn delay(message: NetworkMessage) -> NetworkMessage {
        sleep(Duration::from_millis(30)).await;
        message
    }
}

pub struct NetworkSender {
    // Channel for communication between NetworkSender and other threads.
    transmit: Receiver<NetworkMessage>,

    // Channel for communication between NetworkSender and NetworkRetransmitter
    retransmit: Sender<(NetworkMessage, SocketAddr)>,
}

impl NetworkSender {
    pub fn new(
        transmit: Receiver<NetworkMessage>,
        retransmit: Sender<(NetworkMessage, SocketAddr)>,
    ) -> Self {
        Self {
            transmit,
            retransmit,
        }
    }

    // Kepp one TCP connection per peer, handled by a seperate thread. Communication is done via
    // dedicated channels for every worker.
    pub async fn run(&mut self) {
        // Keep track of workers. Maps socket address to sender channel for worker.
        let mut senders = HashMap::<SocketAddr, Sender<NetworkMessage>>::new();

        // Receive messages from channel.
        while let Some(m) = self.transmit.recv().await {
            for address in &m.addresses {
                // Look up socket address of receiver in hash map.
                let spawn = match senders.get(&address) {
                    // If entry in hash map exists use the channel to send the message to the worker. If
                    // there is an error with the channel spawn a new worker for the receiver socket
                    // address.
                    Some(tx) => tx.send(m.clone()).await.is_err(),
                    // If there is no entry spawn a new worker for the receiver socket address.
                    None => true,
                };

                if spawn {
                    // Spawn a new worker for the receiver socket address.
                    let (tx_ok, rx_ok) = oneshot::channel();
                    let tx = Self::spawn_worker(*address, self.retransmit.clone(), tx_ok).await;

                    let mut retransmit = false;

                    match rx_ok.await {
                        Ok(res) => {
                            match res {
                                true => {
                                    // Send the new worker the message via a channel.
                                    if let Ok(()) = tx.send(m.clone()).await {
                                        // If sending was successful put the channel into the hash map.
                                        senders.insert(*address, tx);
                                    }
                                }
                                false => {
                                    println!("Worker failed to connect");
                                    retransmit = true;
                                }
                            }
                        }
                        Err(_) => {
                            println!("Failed to spawn worker");
                            retransmit = true;
                        }
                    }

                    if retransmit {
                        self.retransmit
                            .send((m.clone(), address.clone()))
                            .await
                            .unwrap();
                    }
                }
            }
        }
    }

    async fn spawn_worker(
        address: SocketAddr,
        retransmit: Sender<(NetworkMessage, SocketAddr)>,
        ok: oneshot::Sender<bool>,
    ) -> Sender<NetworkMessage> {
        // Create channel for communication with NetworkSender.
        let (tx, mut rx): (Sender<NetworkMessage>, Receiver<NetworkMessage>) = channel(10_000);

        tokio::spawn(async move {
            // Connect to provided socket address.
            let stream = match TcpStream::connect(address).await {
                Ok(stream) => {
                    println!("Outgoing connection established with {}", address);
                    let _ = ok.send(true);
                    stream
                }
                // If the connection fails return. This means this worker thread is killed. Therefore
                // using the above created channel will fail. Because of this a new worker will be
                // spawned by the NetworkSender.
                Err(e) => {
                    println!("Failed to connect to {}: {}", address, e);
                    let _ = ok.send(false);
                    return;
                }
            };

            // Frame the TCP stream.
            let mut transport = Framed::new(stream, LengthDelimitedCodec::new());

            // Continuously listen to messages passed to the above created channel.
            while let Some(message) = rx.recv().await {
                // Serialize message
                let bytes = Bytes::from(bincode::serialize(&message).expect("Failed to serialize"));

                // Send the message to the nework
                match transport.send(bytes).await {
                    Ok(_) => println!("Successfully sent message to {}", address),
                    Err(e) => {
                        println!("Failed to send message to {}: {}", address, e);
                        retransmit
                            .send((message.clone(), address.clone()))
                            .await
                            .unwrap();
                        return;
                    }
                }
            }
        });
        tx
    }
}

pub struct NetworkReceiver {
    // Our own network address.
    address: SocketAddr,

    // Channel where received messages are put in.
    deliver: Sender<NetworkMessage>,
}

impl NetworkReceiver {
    pub fn new(address: SocketAddr, deliver: Sender<NetworkMessage>) -> Self {
        Self { address, deliver }
    }

    // Spawn a new worker for each incoming request. This worker is responsible for
    // receiving messages from exactly one connection and forwards those messages to
    // the deliver channel.
    pub async fn run(&self) {
        let listener = TcpListener::bind(&self.address)
            .await
            .expect("Failed to bind TCP port");

        println!("Listening on {}", self.address);

        // Continuously accept new incoming connections.
        loop {
            let (socket, peer) = match listener.accept().await {
                Ok(value) => value,
                // If there is an error with the connection just continue with the loop.
                Err(e) => {
                    println!("{}", e);
                    continue;
                }
            };
            println!("incoming connection established with {}", peer);
            // Spawn a new worker that handles the just established connection.
            Self::spawn_worker(socket, peer, self.deliver.clone()).await;
        }
    }

    async fn spawn_worker(socket: TcpStream, peer: SocketAddr, deliver: Sender<NetworkMessage>) {
        tokio::spawn(async move {
            // Frame the TCP stream.
            let mut transport = Framed::new(socket, LengthDelimitedCodec::new());

            // Continuously receive incoming data from the framed TCP stream.
            while let Some(frame) = transport.next().await {
                match frame {
                    Ok(m) => {
                        // Deserialize received message.
                        let message = bincode::deserialize(&m.freeze()).unwrap();
                        match deliver.send(message).await {
                            Ok(_) => (),
                            Err(e) => println!("{}", e),
                        }
                    }
                    // If there is some error with the framed TCP stream return. This will
                    // kill the worker thread.
                    Err(e) => {
                        println!("{}", e);
                        return;
                    }
                }
            }
            println!("Connection closed by peer {}", peer);
        });
    }
}
