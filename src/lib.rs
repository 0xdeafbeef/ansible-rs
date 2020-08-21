use anyhow::Error;
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use rayon::prelude::*;
use serde::Serialize;
use smol::future::FutureExt;
use smol::{io, Async, Timer};
use ssh2::Session;
mod modules;

use std::fmt::{Debug, Display};
use std::io::Read;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::thread::spawn;
use std::time::{Duration, Instant};
use std_semaphore::Semaphore;
use modules::ModuleProps;
use std::collections::HashMap;

#[derive(Serialize, Debug, Clone)]
pub struct Response {
    pub result: String,
    pub hostname: String,
    pub process_time: Duration,
    pub status: bool,
}

#[derive(Clone)]
pub struct ParallelSshProps {
    tcp_connections_pool: Arc<Semaphore>,
    agent_connections_pool: Arc<Semaphore>,
    timeout_socket: Duration,
    timeout_ssh: Duration,
    sender: Sender<Response>,
    tcp_threads_number: isize,
}

impl Default for ParallelSshPropsBuilder {
    fn default() -> Self {
        Self {
            maximum_connections: Some(Arc::new(Semaphore::new(100))),
            agent_parallelism: Some(Arc::new(Semaphore::new(3))),
            timeout_socket: Some(Duration::from_millis(200)),
            timeout_ssh: Some(Duration::from_secs(120)),
            tcp_threads_number: Some(10),
        }
    }
}

impl ParallelSshPropsBuilder {
    pub fn tcp_connections_pool(&mut self, a: isize) -> &mut Self {
        let mut new = self;
        let sem = Semaphore::new(a);
        new.maximum_connections = Some(Arc::new(sem));
        new.tcp_threads_number = Some(a);
        new
    }
    pub fn agent_connections_pool(&mut self, a: isize) -> &mut Self {
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
                tcp_connections_pool: self
                    .maximum_connections
                    .clone()
                    .ok_or("maximum_connections must be initialized")?,
                agent_connections_pool: self
                    .agent_parallelism
                    .clone()
                    .ok_or("agent_parallelism must be initialized")?,
                tcp_threads_number: self
                    .tcp_threads_number
                    .clone()
                    .ok_or("maximum_connections must be initialized")?,
                sender: tx,
            },
        ))
    }
}

#[derive(Clone)]
pub struct ParallelSshPropsBuilder {
    maximum_connections: Option<Arc<Semaphore>>,
    agent_parallelism: Option<Arc<Semaphore>>,
    timeout_socket: Option<Duration>,
    timeout_ssh: Option<Duration>,
    tcp_threads_number: Option<isize>,
}

fn process_host<A>(
    hostname: String,
    ip: Result<SocketAddr, Error>,
    command: String,
    agent_pool: Arc<Mutex<()>>,
    tx: Sender<Response>,
) where
    A: ToSocketAddrs + Display + Sync + Clone + Send + Debug,
{
    let hostname = match ip {
        Ok(a) => a,
        Err(e) => {
            if let Err(_e) = tx.send(Response {
                result: e.to_string(),
                hostname: hostname.clone(),
                process_time: Default::default(),
                status: false,
            }) {
                eprintln!("Error sending result for {}", hostname);
            }
            return;
        }
    };
    let start_time = Instant::now();
    let result: Result<String, Error> =
        process_host_inner(hostname.clone(), command, agent_pool.clone());
    let process_time = Instant::now() - start_time;
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
    if let Err(e) = tx.send(res) {
        eprintln!("Error sending to channel: {}", e)
    }
    // event!(`
    //     Level::INFO,
    //     "processed :{}, id: {:#?}\nAGENT: {}\n",
    //     hostname,
    //     thread::current().id(),
    //     agent_pool.available_permits()
    // );
}

fn process_host_inner<A>(
    ip: A,
    command: String,
    agent_pool: Arc<Mutex<()>>,
) -> Result<String, Error>
where
    A: ToSocketAddrs + Display + Sync + Clone + Send + Debug,
{
    const TIMEOUT: u32 = 60000;

    let tcp = TcpStream::connect(ip)?;
    let mut sess =
        Session::new().map_err(|_e| Error::msg("Error initializing session".to_string()))?;
    sess.set_tcp_stream(tcp);
    sess.set_timeout(TIMEOUT);
    sess.handshake()
        .map_err(|e| Error::msg(format!("Failed establishing handshake: {}", e)))?;
    let guard = agent_pool.lock();
    sess.userauth_agent("scan")
        .map_err(|e| Error::msg(format!("Error connecting via agent: {}", e)))?;
    drop(guard);
    let mut channel = sess
        .channel_session()
        .map_err(|e| Error::msg(format!("Failed opening channel: {}", e)))?;
    channel
        .exec(&command)
        .map_err(|e| Error::msg(format!("Failed executing command in channel: {}", e)))?;
    let mut channel_buffer = String::with_capacity(4096);
    channel
        .stream(0)
        .read_to_string(&mut channel_buffer)
        .map_err(|e| Error::msg(format!("Error reading result of work: {}", e)))?;
    Ok(channel_buffer)
}

async fn check_host<A>(hostname: A) -> Result<SocketAddr, Error>
where
    A: Display + ToSocketAddrs + Send + Sync + Clone + Debug,
{
    let address = &hostname
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| Error::msg("Failed converting address"))?;
    let address: SocketAddr = address.clone();

    let _tcp = Async::<TcpStream>::connect(address.clone())
        .or(async {
            Timer::new(Duration::from_millis(200)).await;
            Err(io::ErrorKind::TimedOut.into())
        })
        .await?;
    Ok(address)
}

fn check_hosts<A, I>(hosts: I, tx: Sender<(String, String, Result<SocketAddr, Error>)>)
where
    A: Display + ToSocketAddrs + Send + Sync + Clone + Debug,
    I: IntoIterator<Item = (A, String)>,
{
    smol::run(async {
        for (host, command) in hosts {
            let res = check_host(&host).await;
            if let Err(e) = tx.send((host.to_string(), command.parse().unwrap(), res)) {
                eprintln!("Error transmitting ip address between threads: {}", e)
            }
        }
    })
}
impl ParallelSshProps {
    pub fn parallel_command_evaluation<A: 'static, I: Iterator+ 'static>(&self, hosts: I)
    where
        A: Display + ToSocketAddrs + Send + Sync + Clone + Debug,
        I: IntoIterator<Item = (A, String)> + Send,
    {
        let lookup_table: HashMap<_,_> = hosts.cloned().collect();
        let (tx, rx) = bounded(self.tcp_threads_number as usize * 2);
        spawn(move || check_hosts(hosts, tx.clone()));

        let agent_pool = Arc::new(std::sync::Mutex::new(()));

        rx.into_iter()
            .par_bridge()
            .map(|(hostname, command, ip)| {
                process_host::<SocketAddr>(
                    hostname,
                    ip,
                    command,
                    agent_pool.clone(),
                    self.sender.clone(),
                )
            })
            .for_each(|x| drop(x));
    }
    pub fn parallel_module_evaluation<A: 'static, I: 'static, LIST: 'static>(&self, hosts: I, module:  LIST)
        where
            A: Display + ToSocketAddrs + Send + Sync + Clone + Debug,
            I: IntoIterator<Item = A> + Send,
            LIST : IntoIterator<Item=ModuleProps> +Send
    {
        let (tx, rx) = bounded(self.tcp_threads_number as usize * 2);
        spawn(move || check_hosts(hosts, tx.clone()));
        let agent_pool = Arc::new(std::sync::Mutex::new(()));
    }

}
