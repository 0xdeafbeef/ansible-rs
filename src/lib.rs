use async_ssh2::{Error, PublicKey, Session};
use clap::{App, Arg};
use humantime::format_duration;
use serde::Serialize;
use std::fmt::Display;
use std::fs;
use std::fs::File;
use std::io::prelude::*;
use std::io::{BufReader, Read};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{mpsc, Arc};
use std::time::Instant;
use tokio::prelude::*;
use tokio::runtime::{Builder, Runtime};
use tokio::sync::Semaphore;

#[derive(Serialize, Debug, Clone)]
pub struct Response {
    result: String,
    hostname: String,
    process_time: String,
    status: bool,
}

async fn process_host<A>(
    hostname: A,
    command: Arc<String>,
    tx: SyncSender<Response>,
    connection_pool: Arc<Semaphore>,
) -> Response
where
    A: ToSocketAddrs + Display,
{
    let guard = connection_pool.acquire().await;
    let start_time = Instant::now();
    let tcp = match TcpStream::connect(&hostname) {
        Ok(a) => a,
        Err(e) => return construct_error(&hostname, start_time, e.to_string(), &tx),
    };
    dbg!(format!("Tcp connection intialized {}", &hostname));
    let mut sess = match Session::new() {
        Ok(a) => a,
        Err(e) => return construct_error(&hostname, start_time, e.to_string(), &tx),
    };
    const TIMEOUT: u32 = 6000;
    sess.set_timeout(TIMEOUT);
    dbg!(format!("Session is built {}", &hostname));
    if let Err(e) = sess.set_tcp_stream(tcp) {
        return construct_error(&hostname, start_time, e.to_string(), &tx);
    };
    dbg!(format!("Tcp stream is set : {}", &hostname));

    match sess.handshake().await {
        Ok(a) => {
            dbg!(format!("Handshake is done: {}", &hostname));
            a
        }
        Err(e) => {
            dbg!("Handhaske is failed");
            return construct_error(&hostname, start_time, e.to_string(), &tx);
        }
    };
    dbg!(format!("Handshake is done: {}", &hostname));
    // Try to authenticate with the first identity in the agent.
    let mut agent = match sess.agent() {
        Ok(a) => a,
        Err(e) => {
            return construct_error(&hostname, start_time, e.to_string(), &tx);
        }
    };
    match agent.connect().await {
        Ok(_) => (),
        Err(e) => {
            return construct_error(&hostname, start_time, e.to_string(), &tx);
        }
    };
    if let Err(e) = sess.userauth_agent("scan").await {
        return construct_error(&hostname, start_time, e.to_string(), &tx);
    };

    let mut channel = match sess.channel_session().await {
        Ok(a) => a,
        Err(e) => {
            return construct_error(&hostname, start_time, e.to_string(), &tx);
        }
    };
    if let Err(e) = channel.exec(&command).await {
        return construct_error(&hostname, start_time, e.to_string(), &tx);
    };
    let mut s = String::new();
    if let Err(e) = channel.read_to_string(&mut s).await {
        return construct_error(&hostname, start_time, e.to_string(), &tx);
    };
    let end_time = Instant::now();
    let response = Response {
        hostname: hostname.to_string(),
        result: "".to_string(),
        process_time: format_duration(end_time - start_time).to_string(),
        status: true,
    };
    match tx.send(response.clone()) {
        Ok(_) => (),
        Err(e) => eprintln!("Error sending response {}", e),
    };
    response
}

fn construct_error<A>(
    hostname: &A,
    start_time: Instant,
    e: String,
    tx: &SyncSender<Response>,
) -> Response
where
    A: Display + ToSocketAddrs,
{
    dbg!("Constructing error");
    let response = Response {
        result: e,
        hostname: hostname.to_string(),
        process_time: format_duration(Instant::now() - start_time).to_string(),
        status: false,
    };
    match tx.send(response.clone()) {
        Ok(_) => (),
        Err(e) => eprintln!("Error sending response {}", e),
    }
    response
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

    pub fn new(mut self, max_connections: usize) -> ParallelSshProps {
        self.maximum_connections = max_connections;
        self
    }

    pub fn parallel_ssh_process<A>(mut self, hosts: Vec<A>, command: &str) -> Receiver<Response>
    where
        A: Display + ToSocketAddrs,
    {
        let num_of_threads = Arc::new(Semaphore::new(self.maximum_connections));
        let (tx, rx): (SyncSender<Response>, Receiver<Response>) = mpsc::sync_channel(0);
        hosts
            .into_iter()
            .map(|host| {
                smol::Task::spawn( process_host(
                    host.to_string(),
                    Arc::new(command.to_string()),
                    tx.clone(),
                    num_of_threads.clone(),
                )).detach();
            })
            .collect();
        rx
    }
}
