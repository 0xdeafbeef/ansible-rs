use crate::misc::Response;
use humantime::format_duration;
use ssh2::Error;
use ssh2::{Channel, Session};
use std::fmt::Display;
use std::io::Read;
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::time::Instant;
use std_semaphore::Semaphore;
use tempfile::tempfile;
use xz2::read::{XzDecoder, XzEncoder};

pub fn construct_error<A>(
    hostname: &A,
    start_time: Instant,
    e: String,
    tx: &SyncSender<Response>,
    benchmark_mode: bool,
) -> Response
where
    A: Display + ToSocketAddrs,
{
    let response = Response {
        result: e,
        hostname: hostname.to_string(),
        process_time: (Instant::now() - start_time).as_millis().to_string(),
        status: false,
    };
    if !benchmark_mode {
        match tx.send(response.clone()) {
            Ok(_) => (),
            Err(e) => eprintln!("Error sending response {}", e),
        }
    }
    response
}

pub fn process_host(
    host_ip: Ipv4Addr,
    command: &str,
    tx: SyncSender<Response>,
    agent_lock: Arc<Semaphore>,
    timeout: u32,
    benchmark_mode: bool,
) -> Response {
    let start_time = Instant::now();
    let hostname = dbg!(SocketAddrV4::new(host_ip, 22));
    let tcp = match TcpStream::connect(&hostname) {
        Ok(a) => a,
        Err(e) => {
            return construct_error(&hostname, start_time, e.to_string(), &tx, benchmark_mode)
        }
    };
    let mut sess = match Session::new() {
        Ok(a) => a,
        Err(e) => {
            return construct_error(&hostname, start_time, e.to_string(), &tx, benchmark_mode)
        }
    };
    sess.set_timeout(timeout);
    sess.set_tcp_stream(tcp);
    match sess.handshake() {
        Ok(a) => a,
        Err(e) => {
            return construct_error(&hostname, start_time, e.to_string(), &tx, benchmark_mode);
        }
    };
    let guard = agent_lock.access();
    // Try to authenticate with the first identity in the agent.
    match sess.userauth_agent("scan") {
        Ok(_) => (),
        Err(e) => {
            return construct_error(&hostname, start_time, e.to_string(), &tx, benchmark_mode);
        }
    };
    drop(guard);
    let mut channel = match sess.channel_session() {
        Ok(a) => a,
        Err(e) => {
            return construct_error(&hostname, start_time, e.to_string(), &tx, benchmark_mode);
        }
    };
    channel.exec(command).unwrap();
    let mut s = String::new();
    if let Err(e) = channel.read_to_string(&mut s) {
        return construct_error(&hostname, start_time, e.to_string(), &tx, benchmark_mode);
    };
    let response = Response {
        hostname: hostname.to_string(),
        result: s,
        process_time: (Instant::now() - start_time).as_millis().to_string(),
        status: true,
    };
    if !benchmark_mode {
        match tx.send(response.clone()) {
            Ok(_) => (),
            Err(e) => eprintln!("Error sending response {}", e),
        };
    }
    response
}
