extern crate rayon_logs as rayon;

use clap::{App, Arg};
use ssh2::{Session, Agent};
use std::net::{TcpStream, ToSocketAddrs};
use std::io::{Read, BufReader};
use std::path::Path;
use std::io::prelude::*;
use std::fs::File;
use std::slice::{Chunks, ChunksMut};
use rayon::prelude::*;
use std::fmt::Display;
use rayon::ThreadPoolBuilder;


fn process_host<A>(hostname: A) -> Result<String, String>
    where A: ToSocketAddrs + Display {
    let tcp = match TcpStream::connect(&hostname) {
        Ok(a) => a,
        Err(e) => {
            return Err(format!("{}:{}", hostname, e).to_string());
        }
    };
    let mut sess = match Session::new() {
        Ok(a) => a,
        Err(e) => {
            // todo logging
            return Err(format!("{}:{}", hostname, e).to_string());
        }
    };
    sess.set_tcp_stream(tcp);
    match sess.handshake() {
        Ok(a) => a,
        Err(e) => {
            return Err(format!("{}:{}", hostname, e).to_string());
        }
    };

// Try to authenticate with the first identity in the agent.
    match sess.userauth_agent("scan") {
        Ok(a) => a,
        Err(e) => {
            return Err(format!("{}:{}", hostname, e).to_string());
        }
    };
    let mut channel = match sess.channel_session() {
        Ok(a) => a,
        Err(e) => {
            return Err(format!("{}:{}", hostname, e).to_string());
        }
    };
    channel.exec("uptime").unwrap();
    let mut s = String::new();
    match channel.read_to_string(&mut s) {
        Err(e) => {
            return Err(format!("{}:{}", hostname, e).to_string());
        }
        _ => ()
    };
    Ok(format!("{}:{}", hostname, s))
}

fn hosts_builder(path: &Path) -> Vec<String> {
    let file = File::open(path)
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
    let hosts = hosts_builder(Path::new(&args.value_of("config").unwrap()));
//    rayon::ThreadPoolBuilder::new().num_threads(1).build_global().unwrap();
    let pool = ThreadPoolBuilder::new().num_threads(100).build().expect("failed creating pool");
   pool.install(|| hosts.par_iter()
        .map(|x| process_host(x))
        .for_each(|x| println!("{:?}", x)));

}
