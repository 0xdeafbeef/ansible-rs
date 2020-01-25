use clap::{App, Arg};
use std::fmt::Display;
use std::fs;
use std::fs::File;
use std::io::prelude::*;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::{mpsc, Arc};
use std::sync::mpsc::{Receiver, SyncSender};
use std::time::UNIX_EPOCH;
use std::time::{Duration, Instant};
use color_backtrace;
use humantime::format_duration;
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use async_ssh2::{Error, PublicKey, Session};
use tokio::sync::Semaphore;
use tokio::prelude::*;
use std::net::{TcpStream, ToSocketAddrs};
use tokio::runtime::{Builder, Runtime};
use num_cpus::get;

#[derive(Serialize, Debug, Clone)]
struct Response {
	result: String,
	hostname: String,
	process_time: String,
	status: bool,
}
async fn process_host<A>(hostname: A, command: Arc<String>, tx: SyncSender<Response>, connection_pool: Arc<Semaphore>) -> Response
	where
		A: ToSocketAddrs + Display,
{
	let guard = connection_pool.acquire().await;
	let start_time = Instant::now();
	let tcp = match TcpStream::connect(&hostname)
		{
			Ok(a) => a,
			Err(e) => return construct_error(&hostname, start_time, e.to_string(), &tx)
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

struct ParallelSshProps {
	maximum_connections:usize,
	cores_to_use: usize,
	 reactor: Runtime,
	
}

impl ParallelSshProps{
	fn new(mut self, cores_to_use: usize, max_connections: usize) ->ParallelSshProps{
		self.cores_to_use = cores_to_use;
		self.maximum_connections = max_connections;
		self.reactor = match Builder::new()
			.enable_all()
			.threaded_scheduler()
			.core_threads(self.cores_to_use)
			.build()
			{
				Ok(a) => a,
				Err(e) => panic!("Error constructing tokio-reactor")
			};
		self
	}
	fn parallel_ssh_process<A>(mut self, hosts:Vec<A>, command:&str) -> Receiver<Response>
		where A: Display + ToSocketAddrs
	{
		let num_of_threads = Arc::new(Semaphore::new(self.maximum_connections));
		let (tx, rx): (SyncSender<Response>, Receiver<Response>) = mpsc::sync_channel(0);
		let tasks: Vec<_> = hosts.into_iter().map(|host| {
		self.reactor.spawn(process_host(host.to_string(), Arc::new(command.to_string()), tx.clone(), num_of_threads.clone()))
		}).collect();
		rx
	}
}