use anyhow::Error;
use async_executor::{Executor, Spawner};
use async_ssh2::Session;
use futures::future::join_all;
use futures::stream::FuturesUnordered;
use futures::Future;
use futures_channel::mpsc::{channel, Receiver, Sender};
use serde::Serialize;
use smol::Async;
use std::fmt::Display;
use std::io::Read;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

#[derive(Serialize, Debug, Clone)]
pub struct Response {
    pub result: String,
    pub hostname: String,
    pub process_time: Duration,
    pub status: bool,
}

async fn process_host<A>(
    hostname: A,
    command: Arc<String>,
    connection_pool: Arc<Semaphore>,
) -> Response
where
    A: ToSocketAddrs + Display + Sync + Clone + Send,
{
    let start_time = Instant::now();
    let result = dbg!(process_host_inner(hostname.clone(), command, connection_pool).await);
    let process_time = Instant::now() - start_time;
    let response = match result {
        Ok(a) => Response {
            result: a,
            hostname: hostname.to_string(),
            process_time,
            status: true,
        },
        Err(e) => Response {
            result: e.to_string(),
            hostname: hostname.to_string(),
            process_time,
            status: false,
        },
    };

    response
}

async fn process_host_inner<A>(
    hostname: A,
    command: Arc<String>,
    connection_pool: Arc<Semaphore>,
) -> Result<String, Error>
where
    A: ToSocketAddrs + Display + Sync + Clone + Send,
{
    let guard = connection_pool.acquire().await;
    let sync_stream = TcpStream::connect(&hostname)?;
    let tcp = Async::new(sync_stream)?;
    let mut sess =
        Session::new().map_err(|_e| Error::msg(format!("Error initializing session")))?;
    dbg!("Session initialized");
    const TIMEOUT: u32 = 6000;
    sess.set_timeout(TIMEOUT);
    sess.set_tcp_stream(tcp)?;
    sess.handshake()
        .await
        .map_err(|e| Error::msg(format!("Failed establishing handshake: {}", e)))?;
    dbg!("Handshake done");
    let mut agent = sess
        .agent()
        .map_err(|e| Error::msg(format!("Failed connecting to agent: {}", e)))?;
    agent.connect().await?;
    dbg!("Agent connected");
    drop(guard); //todo test, that it really works
    let mut channel = sess
        .channel_session()
        .await
        .map_err(|e| Error::msg(format!("Failed openning channel: {}", e)))?;
    dbg!("Chanel opened");
    channel
        .exec(&command)
        .await
        .map_err(|e| Error::msg(format!("Failed executing command in channel: {}", e)))?;
    let mut channel_buffer = String::with_capacity(4096);
    channel
        .stream(1)
        .read_to_string(&mut channel_buffer)
        .map_err(|e| Error::msg(format!("Error reading result of work: {}", e)))?;
    Ok(channel_buffer)
}

pub struct ParallelSshProps {
    maximum_connections: usize,
}

impl ParallelSshProps {
    pub fn new(max_connections: usize) -> Self {
        Self {
            maximum_connections: max_connections,
        }
    }

    pub async fn parallel_ssh_process<A: 'static>(
        self,
        hosts: Vec<A>,
        command: &str,
    ) -> FuturesUnordered<impl Future<Output = Response>>
    where
        A: Display + ToSocketAddrs + Send + Sync + Clone,
    {
        let num_of_threads = Arc::new(Semaphore::new(self.maximum_connections));
        let futures = FuturesUnordered::new();
        let command = Arc::new(command.to_string());
        for host in hosts {
            let command = command.clone();
            let process_result = process_host(host, command, num_of_threads.clone());
            futures.push(process_result);
        }
        futures
    }
}
