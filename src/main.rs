use std::collections::BTreeMap;
use std::fmt::Display;
use std::fs;
use std::fs::File;
use std::io::{Read};
use std::io::prelude::*;
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::sync::mpsc::{Receiver, SyncSender};
use std::thread::spawn;
use std::time::Instant;
mod misc;
use misc::*;
use chrono::Utc;
use clap::{App, Arg};
use clap::crate_version;
use color_backtrace;
use humantime::format_duration;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
<<<<<<< HEAD
use rayon::{spawn, ThreadPoolBuilder};
use serde::{Deserialize, Serialize, Serializer};
use ssh2::{Error, PublicKey, Session};
=======
use rayon::ThreadPoolBuilder;
use ssh2::Session;
use std_semaphore::Semaphore;
>>>>>>> master
use std::collections::BTreeMap;
use std::fmt::Display;
use std::fs;
use std::fs::File;
use std::io::{Read};
use std::io::prelude::*;
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::sync::mpsc::{Receiver, SyncSender};
use std::thread::spawn;
use std::time::Instant;
mod misc;
use misc::*;
use chrono::Utc;
use clap::{App, Arg};
use clap::crate_version;
use color_backtrace;
use humantime::format_duration;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use ssh2::Session;
use std_semaphore::Semaphore;


fn construct_error<A>(
    hostname: &A,
    start_time: Instant,
    e: String,
    tx: &SyncSender<Response>,
) -> Response
    where
        A: Display + ToSocketAddrs,
{
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

fn process_host(
    host_ip: Ipv4Addr,
    command: &str,
    tx: SyncSender<Response>,
    agent_lock: Arc<Semaphore>,
    timeout: u32,
) -> Response {
    let start_time = Instant::now();
    let hostname = SocketAddrV4::new(host_ip, 22);
    let tcp = match TcpStream::connect(&hostname) {
        Ok(a) => a,
        Err(e) => return construct_error(&hostname, start_time, e.to_string(), &tx),
    };
    let mut sess = match Session::new() {
        Ok(a) => a,
        Err(e) => return construct_error(&hostname, start_time, e.to_string(), &tx),
    };
    sess.set_timeout(timeout);
    sess.set_tcp_stream(tcp);
    match sess.handshake() {
        Ok(a) => a,
        Err(e) => {
            return construct_error(&hostname, start_time, e.to_string(), &tx);
        }
    };
    let guard = agent_lock.access();
    // Try to authenticate with the first identity in the agent.
    match sess.userauth_agent("scan") {
        Ok(_) => (),
        Err(e) => {
            return construct_error(&hostname, start_time, e.to_string(), &tx);
        }
    };
    drop(guard);
    let mut channel = match sess.channel_session() {
        Ok(a) => a,
        Err(e) => {
            return construct_error(&hostname, start_time, e.to_string(), &tx);
        }
    };
    channel.exec(command).unwrap();
    let mut s = String::new();
    if let Err(e) = channel.read_to_string(&mut s) {
        return construct_error(&hostname, start_time, e.to_string(), &tx);
    };
    let end_time = Instant::now();
    let response = Response {
        hostname: hostname.to_string(),
        result: s,
        process_time: format_duration(end_time - start_time).to_string(),
        status: true,
    };
    match tx.send(response.clone()) {
        Ok(_) => (),
        Err(e) => eprintln!("Error sending response {}", e),
    };
    response
}



fn main() {
    color_backtrace::install();
    let args = App::new("ansible-rs")
        .version(crate_version!())
        .arg(
            Arg::with_name("config")
                .short("c")
                .long("config")
                .help("Path to hosts file")
                .required(false)
                .takes_value(true)
                .default_value("config.toml"),
        )
        .arg(
            Arg::with_name("hosts")
                .long("hosts")
                .help("Path to file with hosts")
                .required(true)
                .takes_value(true),
        )
        .arg(
            Arg::with_name("hosts_format")
                .short("f")
                .long("format")
                .takes_value(true)
                .help("Hosts format")
                .long_help("Hosts format: csv for key value and empty(default) for list")
                .default_value(""),
        )
        .get_matches();
    let config = get_config(Path::new(&args.value_of("config").unwrap()));
    let command = &config.command;

    let hosts = if args.value_of("hosts_format").unwrap() == "csv" {
        generate_kv_hosts_from_csv(&args.value_of("hosts").unwrap()).unwrap()
    } else {
        let mut map = BTreeMap::new();
        for h in hosts_builder(Path::new(&args.value_of("hosts").unwrap())) {
            map.insert(h, command.clone());
        }
        map
    };
    dbg!(&config);
    ThreadPoolBuilder::new()
        .num_threads(config.threads)
        .build_global()
        .expect("failed creating pool");
    let (tx, rx): (SyncSender<Response>, Receiver<Response>) = mpsc::sync_channel(0);
    let props = config.output.clone();
    let queue_len = hosts.len() as u64;
    let datetime = Utc::now().format("%H_%M_%S").to_string();
    let incremental_name = format!("{}", &datetime);
    let inc_for_closure = incremental_name.clone();
    let incremental_save_handle =
        spawn(move || incremental_save(rx, &props, queue_len, incremental_name.as_str()));
    let agent_parallelism = Arc::new(Semaphore::new(config.agent_parallelism));
    let timeout = config.timeout * 1000;
    let result: Vec<Response> = hosts
        .par_iter()
        .map(|data| {
            process_host(
                *data.0,
                data.1,
                tx.clone(),
                agent_parallelism.clone(),
                timeout,
            )
        })
        .collect();
    incremental_save_handle
        .join()
        .expect("Error joining threads");
    if config.output.save_to_file {
        save_to_file(&config, result);
    } else {
        save_to_console(&config, &result);
    }
    match config.output.keep_incremental_data {
        Some(true) => {}
        Some(false) => {
            match std::fs::remove_file(Path::new(&inc_for_closure)) {
                Ok(_) => (),
                Err(e) => eprintln!("Error removing temp file : {}", e),
            };
        }
        None => {
            match std::fs::remove_file(Path::new(&inc_for_closure)) {
                Ok(_) => (),
                Err(e) => eprintln!("Error removing temp file : {}", e),
            };
        }
    };
}

fn progress_bar_creator(queue_len: u64) -> ProgressBar {
    let total_hosts_processed = ProgressBar::new(queue_len);
    let total_style = ProgressStyle::default_bar()
        .template("{eta_precise} {wide_bar} Hosts processed: {pos}/{len} Speed: {per_sec} {msg}")
        .progress_chars("##-");
    total_hosts_processed.set_style(total_style);

    total_hosts_processed
}

fn incremental_save(rx: Receiver<Response>, props: &OutputProps, queue_len: u64, filename: &str) {
    let store_dir_date = Utc::today().format("%d_%B_%Y").to_string();
    if !Path::new(&store_dir_date).exists() {
        std::fs::create_dir(Path::new(&store_dir_date))
            .expect("Failed creating dir for temporary save");
    }
    let incremental_name =
        PathBuf::from(store_dir_date.clone() + "/incremental_" + &filename + ".json");
    let mut file = match File::create(incremental_name) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("incremental salving failed. : {}", e);
            return;
        }
    };
    let incremental_hosts_name =
        PathBuf::from(store_dir_date + &"/failed_hosts_".to_string() + filename + ".txt");

    let mut failed_processing_due_to_our_side_error = match File::create(&incremental_hosts_name) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("incremental salving failed. : {}", e);
            return;
        }
    };
    let total = progress_bar_creator(queue_len);
    let mut ok = 0;
    let mut ko = 0;
    file.write_all(b"[\r\n")
        .expect("Writing for incremental saving failed");
    for _ in 0..=queue_len - 1 {
        let received = match rx.recv() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("incremental_save: {}", e);
                break;
            }
        };
        if received.status {
            ok += 1
        } else {
            ko += 1
        };
        if !received.status {
            let hostname = received.hostname.split(':').collect::<Vec<&str>>()[0];
            let error_string = received.result.as_str();
            if error_string.contains("[-42]") || error_string.contains("[-19]") {
                failed_processing_due_to_our_side_error
                    .write_all(&hostname.as_bytes())
                    .expect("Error writing for inc save");
                failed_processing_due_to_our_side_error
                    .write_all(b"\n")
                    .expect("Error writing for inc save");
                continue;
            }
        };
        total.inc(1);
        total.set_message(&format!("OK: {}, Failed: {}", ok, ko));
        let mut data = serde_json::to_string_pretty(&received).unwrap();
        data += ",\n";
        file.write_all(data.as_bytes())
            .expect("Writing for incremental saving failed");
    }
    file.write_all(b"\n]")
        .expect("Writing for incremental saving failed");
    dbg!(&incremental_hosts_name);
    if fs::metadata(&incremental_hosts_name)
        .expect("Error removing temp file")
        .len()
        == 0
    {
        if let Err(e) = fs::remove_file(incremental_hosts_name) {
            eprintln!("Error removing temp file: {}", e);
        }
    }
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

<<<<<<< HEAD
fn process_host<A>(hostname: A, command: &str, tx: SyncSender<Response>) -> Response
    where
        A: ToSocketAddrs + Display,
{
=======
fn process_host(
    host_ip: Ipv4Addr,
    command: &str,
    tx: SyncSender<Response>,
    agent_lock: Arc<Semaphore>,
    timeout: u32,
) -> Response {
>>>>>>> master
    let start_time = Instant::now();
    let hostname = SocketAddrV4::new(host_ip, 22);
    let tcp = match TcpStream::connect(&hostname) {
        Ok(a) => a,
        Err(e) => return construct_error(&hostname, start_time, e.to_string(), &tx),
    };
    let mut sess = match Session::new() {
        Ok(a) => a,
        Err(e) => return construct_error(&hostname, start_time, e.to_string(), &tx),
    };
    sess.set_timeout(timeout);
    sess.set_tcp_stream(tcp);
    match sess.handshake() {
        Ok(a) => a,
        Err(e) => {
            return construct_error(&hostname, start_time, e.to_string(), &tx);
        }
    };
    let guard = agent_lock.access();
    // Try to authenticate with the first identity in the agent.
    match sess.userauth_agent("scan") {
        Ok(_) => (),
        Err(e) => {
            return construct_error(&hostname, start_time, e.to_string(), &tx);
        }
    };
    drop(guard);
    let mut channel = match sess.channel_session() {
        Ok(a) => a,
        Err(e) => {
            return construct_error(&hostname, start_time, e.to_string(), &tx);
        }
    };
    channel.exec(command).unwrap();
    let mut s = String::new();
    if let Err(e) = channel.read_to_string(&mut s) {
        return construct_error(&hostname, start_time, e.to_string(), &tx);
    };
    let end_time = Instant::now();
    let response = Response {
        hostname: hostname.to_string(),
        result: s,
        process_time: format_duration(end_time - start_time).to_string(),
        status: true,
    };
    match tx.send(response.clone()) {
        Ok(_) => (),
        Err(e) => eprintln!("Error sending response {}", e),
    };
    response
}

<<<<<<< HEAD

fn hosts_builder(path: &Path) -> Vec<String> {
    let file = File::open(path).expect("Unable to open the file");
    let reader = BufReader::new(file);
    reader
        .lines()
        .map(|l| l.unwrap_or("Error reading line".to_string()))
        .map(|l| l.replace("\"", ""))
        .map(|l| l.replace("'", ""))
        .map(|l| l + ":22")
        .collect::<Vec<String>>()
}

#[cfg(debug_asserions)]
fn process_host_test<A>(hostname: A, command: &str, tx: SyncSender<Response>) -> Response
    where
        A: Display + ToSocketAddrs,
{
    use rand::prelude::*;
    use std::thread::sleep;
    let start_time = Instant::now();
    let mut rng = rand::rngs::OsRng;
    let stat: bool = rng.gen();
    let wait_time = rng.gen_range(2, 15);
    sleep(Duration::from_secs(wait_time));
    if !stat {
        return construct_error(
            &hostname,
            start_time,
            "Proizoshel trolling".to_string(),
            &tx,
        );
    }
    let end_time = Instant::now();
    let response = Response {
        hostname: hostname.to_string(),
        result: "test".to_string(),
        process_time: format_duration(end_time - start_time).to_string(),
        status: true,
    };
    match tx.send(response.clone()) {
        Ok(_) => (),
        Err(e) => eprintln!("Error sending data via channel: {}", e),
    };
    response
}

#[derive(Deserialize, Debug, Clone)]
struct OutputProps {
    save_to_file: bool,
    filename: Option<String>,
    pretty_format: bool,
    show_progress: bool,
    keep_incremental_data: Option<bool>,
}

#[derive(Deserialize, Debug, Clone)]
struct Config {
    threads: usize,
    output: OutputProps,
    command: String,
    timeout: u32,
}

impl Default for OutputProps {
    fn default() -> Self {
        OutputProps {
            save_to_file: false,
            filename: None,
            pretty_format: false,
            show_progress: false,
            keep_incremental_data: Some(false),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            threads: 10,
            command: String::default(),
            output: OutputProps::default(),
            timeout: 60,
        }
    }
}

fn get_config(path: &Path) -> Config {
    let f = match fs::read_to_string(path) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Failed reading config. Using default values : {}", e);
            return Config::default();
        }
    };
    match toml::from_str(f.as_str()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Error parsing config:{}", e);
            Config::default()
        }
    }
}
=======
>>>>>>> master


#[derive(Deserialize, Debug, Clone)]
struct HostProps {
    hostname: String,
    commands: Option<Vec<String>>,
}

fn main() {
    color_backtrace::install();
    let args = App::new("ansible-rs")
        .version(crate_version!())
        .arg(
            Arg::with_name("config")
                .short("c")
                .long("config")
                .help("Path to hosts file")
                .required(false)
                .takes_value(true)
                .default_value("config.toml"),
        )
        .arg(
            Arg::with_name("hosts")
                .long("hosts")
                .help("Path to file with hosts")
                .required(true)
                .takes_value(true),
        )
        .arg(
            Arg::with_name("hosts_format")
                .short("f")
                .long("format")
                .takes_value(true)
                .help("Hosts format")
                .long_help("Hosts format: csv for key value and empty(default) for list")
                .default_value(""),
        )
        .get_matches();
<<<<<<< HEAD
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(b';')
        .has_headers(true)
        .from_path(Path::new(&args.value_of("hosts").unwrap())).unwrap();
    let mut  hosts :Vec<HostProps> = vec![];
    for host in rdr.deserialize(){
        hosts.push(host.unwrap());
    };
    hosts.iter().for_each(|x| println!("{:#?}", &x));
    let hosts = hosts_builder(Path::new(&args.value_of("hosts").unwrap()));
=======
>>>>>>> master
    let config = get_config(Path::new(&args.value_of("config").unwrap()));
    let command = &config.command;

    let hosts = if args.value_of("hosts_format").unwrap() == "csv" {
        generate_kv_hosts_from_csv(&args.value_of("hosts").unwrap()).unwrap()
    } else {
        let mut map = BTreeMap::new();
        for h in hosts_builder(Path::new(&args.value_of("hosts").unwrap())) {
            map.insert(h, command.clone());
        }
        map
    };
    dbg!(&config);
    ThreadPoolBuilder::new()
        .num_threads(config.threads)
        .build_global()
        .expect("failed creating pool");
    let (tx, rx): (SyncSender<Response>, Receiver<Response>) = mpsc::sync_channel(0);
    let props = config.output.clone();
    let queue_len = hosts.len() as u64;
    let datetime = Utc::now().format("%H_%M_%S").to_string();
    let incremental_name = format!("{}", &datetime);
    let inc_for_closure = incremental_name.clone();
    let incremental_save_handle =
        spawn(move || incremental_save(rx, &props, queue_len, incremental_name.as_str()));
    let agent_parallelism = Arc::new(Semaphore::new(config.agent_parallelism));
    let timeout = config.timeout * 1000;
    let result: Vec<Response> = hosts
        .par_iter()
        .map(|data| {
            process_host(
                *data.0,
                data.1,
                tx.clone(),
                agent_parallelism.clone(),
                timeout,
            )
        })
        .collect();
    incremental_save_handle
        .join()
        .expect("Error joining threads");
    if config.output.save_to_file {
        save_to_file(&config, result);
    } else {
        save_to_console(&config, &result);
    }
    match config.output.keep_incremental_data {
        Some(true) => {}
        Some(false) => {
            match std::fs::remove_file(Path::new(&inc_for_closure)) {
                Ok(_) => (),
                Err(e) => eprintln!("Error removing temp file : {}", e),
            };
        }
        None => {
            match std::fs::remove_file(Path::new(&inc_for_closure)) {
                Ok(_) => (),
                Err(e) => eprintln!("Error removing temp file : {}", e),
            };
        }
    };
}

fn progress_bar_creator(queue_len: u64) -> ProgressBar {
    let total_hosts_processed = ProgressBar::new(queue_len);
    let total_style = ProgressStyle::default_bar()
        .template("{eta_precise} {wide_bar} Hosts processed: {pos}/{len} Speed: {per_sec} {msg}")
        .progress_chars("##-");
    total_hosts_processed.set_style(total_style);

    total_hosts_processed
}

fn incremental_save(rx: Receiver<Response>, props: &OutputProps, queue_len: u64, filename: &str) {
    let store_dir_date = Utc::today().format("%d_%B_%Y").to_string();
    if !Path::new(&store_dir_date).exists() {
        std::fs::create_dir(Path::new(&store_dir_date))
            .expect("Failed creating dir for temporary save");
    }
    let incremental_name =
        PathBuf::from(store_dir_date.clone() + "/incremental_" + &filename + ".json");
    let mut file = match File::create(incremental_name) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("incremental salving failed. : {}", e);
            return;
        }
    };
    let incremental_hosts_name =
        PathBuf::from(store_dir_date + &"/failed_hosts_".to_string() + filename + ".txt");

    let mut failed_processing_due_to_our_side_error = match File::create(&incremental_hosts_name) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("incremental salving failed. : {}", e);
            return;
        }
    };
    let total = progress_bar_creator(queue_len);
    let mut ok = 0;
    let mut ko = 0;
    file.write_all(b"[\r\n")
        .expect("Writing for incremental saving failed");
    for _ in 0..=queue_len - 1 {
        let received = match rx.recv() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("incremental_save: {}", e);
                break;
            }
        };
        if received.status {
            ok += 1
        } else {
            ko += 1
        };
        if !received.status {
            let hostname = received.hostname.split(':').collect::<Vec<&str>>()[0];
            let error_string = received.result.as_str();
            if error_string.contains("[-42]") || error_string.contains("[-19]") {
                failed_processing_due_to_our_side_error
                    .write_all(&hostname.as_bytes())
                    .expect("Error writing for inc save");
                failed_processing_due_to_our_side_error
                    .write_all(b"\n")
                    .expect("Error writing for inc save");
                continue;
            }
        };
        total.inc(1);
        total.set_message(&format!("OK: {}, Failed: {}", ok, ko));
        let mut data = serde_json::to_string_pretty(&received).unwrap();
        data += ",\n";
        file.write_all(data.as_bytes())
            .expect("Writing for incremental saving failed");
    }
    file.write_all(b"\n]")
        .expect("Writing for incremental saving failed");
    dbg!(&incremental_hosts_name);
    if fs::metadata(&incremental_hosts_name)
        .expect("Error removing temp file")
        .len()
        == 0
    {
        if let Err(e) = fs::remove_file(incremental_hosts_name) {
            eprintln!("Error removing temp file: {}", e);
        }
    }
}
