use anyhow::Error;
use async_ssh2::{PublicKey, Session};
use clap::{App, Arg};
use humantime::format_duration;
use serde::Serialize;
use smol::Async;
use std::fmt::Display;
use std::fs;
use std::fs::File;
use std::io::prelude::*;
use std::io::{BufReader, Read};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::Semaphore;

#[derive(Serialize, Debug, Clone)]
pub struct Response {
    result: String,
    hostname: String,
    process_time: Duration,
}

async fn process_host<A>(
    hostname: A,
    command: Arc<String>,
    mut tx: Sender<Response>,
    connection_pool: Arc<Semaphore>,
) where
    A: ToSocketAddrs + Display + Sync + Clone + Send,
{
    let start_time = Instant::now();
    let result = process_host_inner(hostname.clone(), command, connection_pool).await;
    let process_time = Instant::now() - start_time;
    let response = match result {
        Ok(a) => Response {
            result: a,
            hostname: hostname.to_string(),
            process_time,
        },
        Err(e) => Response {
            result: e.to_string(),
            hostname: hostname.to_string(),
            process_time,
        },
    };
    if let Err(e) = tx.send(response).await {
        eprintln!("Error sending result via channel: {}", e);
    };
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
    let mut sess = Session::new().map_err(|e| Error::msg(format!("Error initializing session")))?;
    const TIMEOUT: u32 = 6000;
    sess.set_timeout(TIMEOUT);
    sess.set_tcp_stream(tcp);
    sess.handshake()
        .await
        .map_err(|e| Error::msg(format!("Failed establishing handshake: {}", e)))?;
    let mut agent = sess
        .agent()
        .map_err(|e| Error::msg(format!("Failed connecting to agent: {}", e)))?;
    agent.connect().await?;
    drop(guard); //todo test, that it really works
    let mut channel = sess
        .channel_session()
        .await
        .map_err(|e| Error::msg(format!("Failed openning channel: {}", e)))?;
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
impl Default for ParallelSshProps {
    fn default() -> Self {
        ParallelSshProps {
            maximum_connections: 1,
        }
    }
}

impl ParallelSshProps {
    pub fn new(max_connections: usize) -> ParallelSshProps {
        Self {
            maximum_connections: max_connections,
        }
    }

    pub fn parallel_ssh_process<A: 'static>(
        mut self,
        hosts: Vec<A>,
        command: &str,
    ) -> Receiver<Response>
    where
        A: Display + ToSocketAddrs + Send + Sync + Clone,
    {
        let num_of_threads = Arc::new(Semaphore::new(self.maximum_connections));
        let (tx, rx): (Sender<Response>, Receiver<Response>) =
            channel(self.maximum_connections * 2); //todo check this
        hosts
            .into_iter()
            .map(|host| {
                smol::Task::spawn(process_host(
                    host,
                    Arc::new(command.to_string()),
                    tx.clone(),
                    num_of_threads.clone(),
                ))
                .detach();
            })
            .for_each(|_| ());
        rx
    }
}
