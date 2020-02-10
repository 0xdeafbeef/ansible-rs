use clap::{App, Arg};
use std::fmt::Display;
use std::fs;
use std::fs::File;
use std::io::prelude::*;
use std::io::{BufReader, Read};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::{mpsc, Arc};
use std::sync::mpsc::{Receiver, SyncSender};
use std::time::UNIX_EPOCH;
use std::time::{Duration, Instant};

use color_backtrace;
use humantime::format_duration;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use rayon::{spawn, ThreadPoolBuilder};
use serde::{Deserialize, Serialize};
use ssh2::Session;
use std_semaphore::Semaphore;
use std::thread::sleep;
use std::collections::BTreeMap;

#[derive(Serialize, Debug, Clone)]
struct Response {
    result: String,
    hostname: String,
    process_time: String,
    status: bool,
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

fn process_host<A>(hostname: A, command: &str, tx: SyncSender<Response>, agent_lock: Arc<Semaphore>, timeout: u32) -> Response
    where
        A: ToSocketAddrs + Display,
{
    let start_time = Instant::now();
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
        Err(e) => { return construct_error(&hostname, start_time, e.to_string(), &tx); }
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
    agent_parallism: isize,
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
            agent_parallism: 1,
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

fn save_to_file(conf: &Config, data: Vec<Response>) {
    let filename = match &conf.output.filename {
        None => {
            eprintln!("Filename to save is not given. Printing to stdout.");
            save_to_console(&conf, &data);
            return;
        }
        Some(a) => Path::new(a.as_str()),
    };

    let file = match File::create(filename) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Erorr saving content to file:{}", e);
            save_to_console(&conf, &data);
            return;
        }
    };
    if conf.output.pretty_format {
        match serde_json::to_writer_pretty(file, &data) {
            Ok(_) => println!("Saved successfully"),
            Err(e) => eprintln!("Error saving: {}", e),
        };
    } else {
        match serde_json::to_writer(file, &data) {
            Ok(_) => println!("Saved successfully"),
            Err(e) => eprintln!("Error saving: {}", e),
        }
    }
}

fn save_to_console(conf: &Config, data: &Vec<Response>) {
    if conf.output.pretty_format {
        println!("{}", serde_json::to_string_pretty(&data).unwrap())
    } else {
        println!("{}", serde_json::to_string(&data).unwrap())
    }
}

fn generate_kv_hosts_from_csv(path: &str) -> Result<BTreeMap<String, String>, std::io::Error>
{
//     let f = File::open(Path::new(path))?;
    let mut rd = csv::ReaderBuilder::new().from_path(Path::new(path))?;
    let mut map = BTreeMap::new();
    for res in rd.records() {
        let rec = res.unwrap();
        let k = rec.get(0).unwrap();
        let v = rec.get(1).unwrap();
        map.insert(k.to_string(), v.to_string());
    }
    Ok(map)
}

fn main() {
    color_backtrace::install();
    let args = App::new("SSH analyzer")
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
        .arg(Arg::with_name("hosts_format")
            .short("f")
            .long("format")
            .takes_value(true)
            .help("Hosts format")
            .long_help("Hosts format: csv for key value and empty(default) for list")
            .default_value(""))
        .get_matches();
    let config = get_config(Path::new(&args.value_of("config").unwrap()));
    let command = &config.command;

    let hosts = if args.value_of("hosts_format").unwrap() == "csv" {
        generate_kv_hosts_from_csv(&args.value_of("hosts").unwrap()).unwrap()
    } else {
        let mut map = BTreeMap::new();
        for h in hosts_builder(Path::new(&args.value_of("hosts").unwrap())) {
            map.insert(h, command.clone());
        };
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
    let datetime = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string();
    let incremental_name = format!("incremental_{}.json", &datetime);
    let inc_for_closure = incremental_name.clone();
    spawn(move || incremental_save(rx, &props, queue_len, incremental_name.as_str()));
    let agent_parallelism = Arc::new(Semaphore::new(config.agent_parallism));
    let timeout = config.timeout * 1000;
    let result: Vec<Response> = hosts
        .par_iter()
        .map(|data| process_host(data.0.to_string() + ":22", data.1, tx.clone(), agent_parallelism.clone(), timeout))
        .collect();
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
    let mut file = match File::create(Path::new(filename)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("incremental salving failed. : {}", e);
            return;
        }
    };
    let mut failed_processing_due_to_our_side_error = match File::create(Path::new(&("failed_hosts_".to_string() + filename))) {
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
    for _ in 0..=queue_len {
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
            let  hostname = received.hostname.split(':').collect::<Vec<&str>>()[0];
            match received.result.as_str() {
                "[-42] failed connecting agent" => {
                    failed_processing_due_to_our_side_error.write_all(&hostname.as_bytes()).expect("Error writing for inc save");
                    failed_processing_due_to_our_side_error.write_all(b"\n").expect("Error writing for inc save");
                    continue;
                }
                "[-19] Callback returned error" => {
                    failed_processing_due_to_our_side_error.write_all(&hostname.as_bytes()).expect("Error writing for inc save");
                    failed_processing_due_to_our_side_error.write_all(b"\n").expect("Error writing for inc save");
                    continue;
                }
                _ => ()
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
}