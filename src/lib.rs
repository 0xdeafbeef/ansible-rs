use anyhow::Error;
use std::thread;
use async_channel::{unbounded, Receiver, Sender};
use async_ssh2::Session;
use futures::prelude::*;
use futures::stream::FuturesUnordered;
use serde::Serialize;
use smol::future::FutureExt;
use smol::io;
use tracing::{event, instrument, Level};
// use smol::Async;
use async_io::{Async, Timer};
use std::fmt::{Debug, Display};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::macros::support::Pin;
use tokio::sync::Semaphore;

#[derive(Serialize, Debug, Clone)]
pub struct Response {
    pub result: String,
    pub hostname: String,
    pub process_time: Duration,
    pub status: bool,
}

#[derive(Debug)]
pub struct ParallelSshProps {
    maximum_connections: Arc<Semaphore>,
    agent_parallelism: Arc<Semaphore>,
    timeout_socket: Duration,
    timeout_ssh: Duration,
    sender: Sender<Response>,
}

impl Default for ParallelSshPropsBuilder {
    fn default() -> Self {
        Self {
            maximum_connections: Some(Arc::new(Semaphore::new(100))),
            agent_parallelism: Some(Arc::new(Semaphore::new(3))),
            timeout_socket: Some(Duration::from_millis(200)),
            timeout_ssh: Some(Duration::from_secs(120)),
        }
    }
}

impl ParallelSshPropsBuilder {
    pub fn maximum_connections(&mut self, a: usize) -> &mut Self {
        let mut new = self;
        let sem = Semaphore::new(a);
        new.maximum_connections = Some(Arc::new(sem));
        new
    }
    pub fn agent_parallelism(&mut self, a: usize) -> &mut Self {
        let mut new = self;
        let sem = Semaphore::new(a);
        new.agent_parallelism = Some(Arc::new(sem));
        new
    }
    pub fn timeout_socket(&mut self, a: Duration) -> &mut Self {
        let mut new = self;
        new.timeout_socket = Some(a);
        new
    }
    pub fn timeout_ssh(&mut self, a: Duration) -> &mut Self {
        let mut new = self;
        new.timeout_ssh = Some(a);
        new
    }
    pub fn build(&self) -> Result<(Receiver<Response>, ParallelSshProps), String> {
        let (tx, rx) = unbounded();
        Ok((
            rx,
            ParallelSshProps {
                timeout_ssh: *self
                    .timeout_ssh
                    .clone()
                    .as_ref()
                    .ok_or("timeout_ssh must be initialized")?,
                timeout_socket: *self
                    .timeout_socket
                    .clone()
                    .as_ref()
                    .ok_or("timeout_socket must be initialized")?,
                maximum_connections: self
                    .maximum_connections
                    .clone()
                    .ok_or("maximum_connections must be initialized")?,
                agent_parallelism: self
                    .agent_parallelism
                    .clone()
                    .ok_or("agent_parallelism must be initialized")?,
                sender: tx,
            },
        ))
    }
}

#[derive(Clone, Debug)]
pub struct ParallelSshPropsBuilder {
    maximum_connections: Option<Arc<Semaphore>>,
    agent_parallelism: Option<Arc<Semaphore>>,
    timeout_socket: Option<Duration>,
    timeout_ssh: Option<Duration>,
}

#[instrument]
async fn process_host<A>(
    hostname: A,
    command: Arc<String>,
    agent_pool: Arc<Semaphore>,
    threads_limit: Arc<Semaphore>,
    tx: Sender<Response>,
) where
    A: ToSocketAddrs + Display + Sync + Clone + Send + Debug,
{
    // event!(Level::INFO, "inside process host");
    let start_time = Instant::now();
    let result = process_host_inner(hostname.clone(), command, agent_pool, threads_limit)
        //     .or(async {
        //     Timer::new(Duration::from_secs(60)).await;
        //         Err( Error::msg("Timed out waiting for ssh-related operations"))
        // })
        .await;
    let process_time = Instant::now() - start_time;
    // println!("Exiting  {}",&hostname);
    let res = match result {
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
    if let Err(e) = tx.send(res).await {
        eprintln!("Error sending to channel: {}", e)
    };
    event!(Level::INFO, "processed :{}, id: {:#?}", hostname,thread::current().id());
}

async fn process_host_inner<A>(
    hostname: A,
    command: Arc<String>,
    agent_pool: Arc<Semaphore>,
    threads_pool: Arc<Semaphore>,
) -> Result<String, Error>
where
    A: ToSocketAddrs + Display + Sync + Clone + Send + Debug,
{
    // println!("Entered process_host_inner for {}",&hostname);
    let _threads_guard = threads_pool.acquire().await;
    // println!("Permits: {}", threads_pool.available_permits());
    // println!("Thread pool acquired for {}",&hostname);
    let address = &hostname
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| Error::msg("Failed converting address"))?;
    // // let sync_stream = TcpStream::connect_timeout(&address, timeout_socket)?;
    // println!("Sync stream created {}",&hostname);
    let tcp = Async::<TcpStream>::connect(*address)
        .or(async {
            Timer::new(Duration::from_millis(200)).await;
            Err(io::ErrorKind::TimedOut.into())
        })
        .await?;
    let mut sess =
        Session::new().map_err(|_e| Error::msg("Error initializing session".to_string()))?;

    // println!("Session established {}",&hostname);
    const TIMEOUT: u32 = 6000;
    sess.set_timeout(TIMEOUT);
    sess.set_tcp_stream(tcp)?;
    sess.handshake()
        .await
        .map_err(|e| Error::msg(format!("Failed establishing handshake: {}", e)))?;
    // dbg!("Handshake done");
    let guard = agent_pool.acquire().await;
    // println!("Permits: {}", agent_pool.available_permits());
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
    let mut reader = blocking::Unblock::new(channel.stream(0));
    let mut channel_buffer = String::with_capacity(4096);
    reader
        .read_to_string(&mut channel_buffer)
        .await
        .map_err(|e| Error::msg(format!("Error reading result of work: {}", e)))?;
    Ok(channel_buffer)
}

impl ParallelSshProps {
    pub fn parallel_ssh_process<A: 'static>(
        &self,
        hosts: Vec<A>,
        command: &str,
    ) -> Vec<Pin<Box<dyn Future<Output = ()> + std::marker::Send>>>
    where
        A: Display + ToSocketAddrs + Send + Sync + Clone + Debug,
    {
        let mut futures = Vec::with_capacity(hosts.len());
        let command = Arc::new(command.to_string());
        dbg!(&self);
        for host in hosts {
            let command = command.clone();
            let process_result = process_host(
                host,
                command,
                self.agent_parallelism.clone(),
                self.maximum_connections.clone(),
                self.sender.clone(),
            );
            futures.push(smol::prelude::FutureExt::boxed(process_result));
        }
        futures
    }
}
