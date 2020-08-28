use anyhow::Error;
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use rayon::prelude::*;
use serde::Serialize;
use smol::future::FutureExt;
use smol::{io, Async, Timer};
use ssh2::{Channel, Session};

use ansible_modules::{AuthType, ConnectionProps};
use ansible_modules::{CommandOutput, ModuleTree};
use std::collections::HashMap;
use std::fmt::{Debug, Display};
use std::io::Read;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc};
use std::thread::spawn;
use std::time::{Duration, Instant};
use std_semaphore::Semaphore;

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
    modules: Option<ModuleTree>,
}

impl Default for ParallelSshPropsBuilder {
    fn default() -> Self {
        Self {
            maximum_connections: Some(Arc::new(Semaphore::new(100))),
            agent_parallelism: Some(Arc::new(Semaphore::new(3))),
            timeout_socket: Some(Duration::from_millis(200)),
            timeout_ssh: Some(Duration::from_secs(120)),
            tcp_threads_number: Some(10),
            module_tree: None,
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
    pub fn set_module_tree(&mut self, a: ModuleTree) -> &mut Self {
        let mut new = self;
        new.module_tree = Some(a);
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
                modules: self.module_tree.clone(),
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
    module_tree: Option<ModuleTree>,
}

impl ConnectionProps for ParallelSshProps {
    fn get_timeout(&self) -> u32 {
        self.timeout_ssh.as_millis() as u32
    }

    fn tcp_synchronization(&self) {
        self.tcp_connections_pool.acquire()
    }

    fn agent_synchronization(&self) {
        self.agent_connections_pool.acquire()
    }

    fn tcp_release(&self) {
        self.tcp_connections_pool.release()
    }

    fn agent_release(&self) {
        self.agent_connections_pool.release()
    }
}

impl ParallelSshProps {
    pub fn parallel_command_evaluation<A: 'static, I: 'static>(&self, hosts: I)
    where
        A: Display + ToSocketAddrs + Send + Sync + Clone + Debug + Eq + std::hash::Hash + ToString,
        I: IntoIterator<Item = (A, String)> + Send,
    {
        let lookup_table: HashMap<String, _> =
            hosts.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let (tx, rx) = bounded(self.tcp_threads_number as usize * 2);
        {
            let hosts: Vec<_> = lookup_table.clone().into_iter().map(|(k, _v)| k).collect();
            spawn(move || Self::check_hosts(hosts, tx.clone()));
        }

        rx.into_iter()
            .par_bridge()
            .map(|(hostname, ip)| {
                let command = &lookup_table
                    .get(&hostname)
                    .expect("infallible operation, but who knows...");
                (hostname, command.clone(), ip)
            })
            .map(|(hostname, command, ip)| {
                self.process_host::<SocketAddr>(None, None, hostname, ip, command.to_string())
            })
            .for_each(|x| drop(x));
    }

    fn send_result(&self, hostname: String, res: Result<CommandOutput, Error>) {
        let (status, result) = match res {
            Ok(a) => (
                true,
                match a {
                    CommandOutput::Multi(map) => serde_json::to_string(&map).unwrap(),
                    CommandOutput::Single(str) => str,
                },
            ),
            Err(e) => (false, e.to_string()),
        };
        if let Err(e) = self.sender.send(Response {
            hostname,
            status,
            result,
            process_time: Duration::from_secs(0),
        }) {
            eprintln!("Error while sending result via a channel: {}", e);
        }
    }

    pub fn parallel_module_evaluation<A: 'static, I: 'static>(&self, hosts: I, module_name: String)
    where
        A: Display + ToSocketAddrs + Send + Sync + Clone + Debug,
        I: IntoIterator<Item = A> + Send,
    {
        let (tx, rx) = bounded(self.tcp_threads_number as usize * 2);
        spawn(move || Self::check_hosts(hosts, tx.clone()));
        let modules = self.modules.clone().expect("Modules are not initialized");
        rx.into_iter()
            .par_bridge()
            .filter_map(|(hostname, ip)| {
                let sock = match ip {
                    Ok(sock) => sock,
                    Err(e) => {
                        if let Err(e) = self.sender.send(Response {
                            result: format!("Failed connecting: {}", e),
                            hostname,
                            process_time: Duration::from_secs(0),
                            status: false,
                        }) {
                            eprintln!("Failed sending result via channel: {}", e);
                        }
                        return None;
                    }
                };
                Some((hostname, sock))
            })
            .map(|(hostname, sock)| {
                (
                    hostname,
                    modules.run_module(
                        &module_name,
                        sock,
                        AuthType::AgentFirst("scan".into()),
                        self as &dyn ConnectionProps,
                    ),
                )
            })
            .for_each(|(hostname, res)| self.send_result(hostname, res));
    }

    fn process_host<HOSTNAME>(
        &self,
        auth: Option<fn(&Session) -> Result<(), Error>>,
        process: Option<Box<dyn FnMut(&mut Channel) -> Result<(), Error>>>,
        hostname: String,
        ip: Result<SocketAddr, Error>,
        command: String,
    ) where
        HOSTNAME: ToSocketAddrs + Display + Sync + Clone + Send + Debug,
    {
        let tx = self.sender.clone();
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
        let auth = match auth {
            Some(a) => a,
            None => |sess: &Session| -> Result<(), Error> {
                let res = sess.userauth_agent("scan");
                if let Err(e) = res {
                    return Err(Error::new(e));
                };
                Ok(())
            },
        };
        let mut process = match process {
            Some(a) => a,
            None => Box::new(|chan: &mut Channel| -> Result<(), Error> {
                let res = chan.exec(&command);
                if let Err(e) = res {
                    return Err(Error::new(e));
                };
                Ok(())
            }),
        };
        let result: Result<String, Error> = Self::process_host_inner(
            hostname.clone(),
            self.agent_connections_pool.clone(),
            auth,
            &mut process,
        );
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

    fn process_host_inner<HOSTNAME>(
        ip: HOSTNAME,
        agent_pool: Arc<Semaphore>,
        auth: fn(&Session) -> Result<(), Error>,
        process: &mut dyn FnMut(&mut Channel) -> Result<(), Error>,
    ) -> Result<String, Error>
    where
        HOSTNAME: ToSocketAddrs + Display + Sync + Clone + Send + Debug,
    {
        const TIMEOUT: u32 = 60000;

        let tcp = TcpStream::connect(ip)?;
        let mut sess =
            Session::new().map_err(|_e| Error::msg("Error initializing session".to_string()))?;
        sess.set_tcp_stream(tcp);
        sess.set_timeout(TIMEOUT);
        sess.handshake()
            .map_err(|e| Error::msg(format!("Failed establishing handshake: {}", e)))?;
        let guard = agent_pool.access();
        auth(&sess).map_err(|e| Error::msg(format!("Authentication Error {}", e)))?;
        sess.userauth_agent("scan")
            .map_err(|e| Error::msg(format!("Error connecting via an agent: {}", e)))?;
        drop(guard);
        let mut channel = sess
            .channel_session()
            .map_err(|e| Error::msg(format!("Failed opening channel: {}", e)))?;
        process(&mut channel)
            .map_err(|e| Error::msg(format!("Failed executing command in a channel: {}", e)))?;
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
    ///checks host and returns `SocketAddr` in case of successful connection
    fn check_hosts<A, I>(hosts: I, tx: Sender<(String, Result<SocketAddr, Error>)>)
    where
        A: Display + ToSocketAddrs + Send + Sync + Clone + Debug,
        I: IntoIterator<Item = A>,
    {
        smol::run(async {
            for host in hosts {
                let res = Self::check_host(&host).await;
                if let Err(e) = tx.send((host.to_string(), res)) {
                    eprintln!("Error transmitting ip address between threads: {}", e)
                }
            }
        })
    }
}
