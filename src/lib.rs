use anyhow::Error;
use async_ssh2::Session;
use futures::prelude::*;
use futures::stream::FuturesUnordered;
use futures::Future;
use serde::Serialize;
use smol::reader;
use smol::Async;
use std::fmt::Display;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

#[macro_use]
extern crate derive_builder;

#[derive(Serialize, Debug, Clone)]
pub struct Response {
    pub result: String,
    pub hostname: String,
    pub process_time: Duration,
    pub status: bool,
}

#[derive(Builder)]
pub struct ParallelSshProps {
    #[builder(setter(skip))]
    maximum_connections: Arc<Semaphore>,
    #[builder(setter(skip))]
    agent_parallelism: Arc<Semaphore>,
    timeout_socket: Duration,
    timeout_ssh: Duration,
}

impl Default for ParallelSshProps {
    fn default() -> Self {
        Self {
            maximum_connections: Arc::new(Semaphore::new(100)),
            agent_parallelism: Arc::new(Semaphore::new(3)),
            timeout_socket: Duration::from_millis(200),
            timeout_ssh: Duration::from_secs(120),
        }
    }
}

impl ParallelSshPropsBuilder {
    fn maximum_connections(&mut self, a: usize) -> &mut Self {
        let mut new = self;
        new.maximum_connections = Arc::new(Semaphore::new(a));
        new
    }
    fn agent_parallelism(&mut self, a: usize) -> &mut Self {
        let mut new = self;
        new.agent_parallelism = Arc::new(Semaphore::new(a));
        new
    }
}

async fn process_host<A>(
    hostname: A,
    command: Arc<String>,
    timeout_socket: Duration,
    agent_pool: Arc<Semaphore>,
    threads_limit: Arc<Semaphore>,
) -> Response
where
    A: ToSocketAddrs + Display + Sync + Clone + Send,
{
    let start_time = Instant::now();
    let result = process_host_inner(
        hostname.clone(),
        timeout_socket,
        command,
        agent_pool,
        threads_limit,
    )
    .await;
    let process_time = Instant::now() - start_time;
    match result {
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
    }
}

async fn process_host_inner<A>(
    hostname: A,
    timeout_socket: Duration,
    command: Arc<String>,
    agent_pool: Arc<Semaphore>,
    threads_pool: Arc<Semaphore>,
) -> Result<String, Error>
where
    A: ToSocketAddrs + Display + Sync + Clone + Send,
{
    let _threads_guard = threads_pool.acquire().await;
    let address = &hostname
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| Error::msg("Failed converting address"))?;

    let sync_stream = TcpStream::connect_timeout(&address, timeout_socket)?;
    let tcp = Async::new(sync_stream)?;
    let mut sess =
        Session::new().map_err(|_e| Error::msg("Error initializing session".to_string()))?;
    // dbg!("Session initialized");
    const TIMEOUT: u32 = 6000;
    sess.set_timeout(TIMEOUT);
    sess.set_tcp_stream(tcp)?;
    sess.handshake()
        .await
        .map_err(|e| Error::msg(format!("Failed establishing handshake: {}", e)))?;
    // dbg!("Handshake done");
    let guard = agent_pool.acquire().await;
    let mut agent = sess
        .agent()
        .map_err(|e| Error::msg(format!("Failed connecting to agent: {}", e)))?;
    agent.connect().await?;
    // dbg!("Agent connected");
    sess.userauth_agent("scan")
        .await
        .map_err(|e| Error::msg(format!("Error connecting via agent: {}", e)))?;
    drop(guard); //todo test, that it really works
    let mut channel = sess
        .channel_session()
        .await
        .map_err(|e| Error::msg(format!("Failed opening channel: {}", e)))?;
    // dbg!("Chanel opened");
    channel
        .exec(&command)
        .await
        .map_err(|e| Error::msg(format!("Failed executing command in channel: {}", e)))?;

    // let mut command_stdout = reader(channel.stream(0));
    let mut reader = reader(channel.stream(0));
    let mut channel_buffer = String::with_capacity(4096);
    reader
        .read_to_string(&mut channel_buffer)
        .await
        .map_err(|e| Error::msg(format!("Error reading result of work: {}", e)))?;
    Ok(channel_buffer)
}

impl ParallelSshProps {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn parallel_ssh_process<A: 'static>(
        &self,
        hosts: Vec<A>,
        command: &str,
    ) -> FuturesUnordered<impl Future<Output = Response>>
    where
        A: Display + ToSocketAddrs + Send + Sync + Clone,
    {
        let futures = FuturesUnordered::new();
        let command = Arc::new(command.to_string());

        for host in hosts {
            let command = command.clone();
            let process_result = process_host(
                host,
                command,
                self.timeout_socket,
                self.agent_parallelism.clone(),
                self.num_of_threads.clone(),
            );
            futures.push(process_result);
        }
        futures
    }
}
