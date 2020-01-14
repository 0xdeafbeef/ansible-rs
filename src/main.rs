#[allow(dead_code)]
use clap::{App, Arg};
use ssh2::{Session, Agent};
use std::net::{TcpStream, ToSocketAddrs};
use std::io::{Read, BufReader};
use std::path::Path;
use evmap;
use std::io::prelude::*;
use std::fs::File;
use std::slice::{Chunks, ChunksMut};
use std::thread::spawn;

const NTHREADS: usize = 2;

fn process_hosts(list: &[String])
{
    for host in list {
        process_host(host);
    }
}

fn process_host<A>(hostname: A)
    where A: ToSocketAddrs {
    let tcp = match TcpStream::connect(hostname) {
        Ok(a) => a,
        Err(e) => {
            return;
        }
    };
    let mut sess = match Session::new() {
        Ok(a) => a,
        Err(e) => {
            // todo logging
            return;
        }
    };
    sess.set_tcp_stream(tcp);
    match sess.handshake() {
        Ok(a) => a,
        Err(e) => {
            return;
        }
    };

// Try to authenticate with the first identity in the agent.
    match sess.userauth_agent("scan") {
        Ok(a) => a,
        Err(e) => {
            return;
        }
    };
    let mut channel = match sess.channel_session() {
        Ok(a) => a,
        Err(e) => {
            return;
        }
    };
    channel.exec("uptime").unwrap();
    let mut s = String::new();
    match channel.read_to_string(&mut s) {
        Err(e) => {
            return;
        }
        _ => ()
    };
    println!("{}", s);
    channel.wait_close();
    println!("{}", channel.exit_status().unwrap());
}

fn hosts_builder(path: &Path) -> Vec<String> {
    let  file = File::open(path)
        .expect("Unable to open the file");
    let reader = BufReader::new(file);
    reader.lines()
        .map(|l| l.unwrap_or("Error reading line".to_string()))
        .map(|l| l.replace("\"", ""))
        .map(|l| l.replace("'", ""))
        .map(|l| l + ":22")
        .collect::<Vec<String>>()
}

fn main() {
    let args = App::new("SSH analyzer")
        .arg(
            Arg::with_name("config")
                .short("c")
                .long("config")
                .help("Path to hosts file")
                .required(true)
                .takes_value(true)
        )
        .get_matches();
    let hosts  = hosts_builder(Path::new(&args.value_of("config").unwrap()));
    let hosts: Vec<&[String]> =hosts.chunks(NTHREADS).collect();
    dbg!(&hosts);
    let mut thread_heandles = vec![];
    for chunk in hosts {
        thread_heandles.push(
            spawn(move || process_hosts(chunk)))
    }

}
