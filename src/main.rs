use ansible_rs::{ParallelSshProps, ParallelSshPropsBuilder, Response};
use chrono::Utc;
use clap::crate_version;
use clap::{App, Arg};
use color_backtrace;
use crossbeam_channel::Receiver;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::prelude::*;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use std::thread::spawn;
use std::time::Duration;

mod misc;
use misc::{generate_kv_hosts_from_csv, get_config, hosts_builder, Config};
use std::net::SocketAddr;

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
                .default_value("./config.toml"),
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
    let config: Config = confy::load_path(args.value_of("config").unwrap()).unwrap();
    let command = &config.command;

    let hosts = if args.value_of("hosts_format").unwrap() == "csv" {
        let hosts = generate_kv_hosts_from_csv(&args.value_of("hosts").unwrap()).unwrap();
        hosts
            .into_iter()
            .map(|(ad, com)| (SocketAddr::new(IpAddr::from(ad), 22), com))
            .collect()
    } else {
        let mut map = BTreeMap::new();
        for h in hosts_builder(Path::new(&args.value_of("hosts").unwrap())) {
            map.insert(SocketAddr::new(IpAddr::from(h), 22), command.clone());
        }
        map
    };
    dbg!(&config);
    ThreadPoolBuilder::new()
        .num_threads(config.threads)
        .build_global()
        .expect("failed creating pool");
    let (channel, ssh_processor): (_, ParallelSshProps) = ParallelSshPropsBuilder::default()
        .agent_connections_pool(config.agent_parallelism)
        .tcp_connections_pool(config.threads as isize)
        .timeout_socket(Duration::from_millis(config.timeout as u64))
        .timeout_ssh(Duration::from_secs(60))
        .build()
        .expect("Failed building ssh_processor instance");
    let len = hosts.len();
    let handler = spawn(move || incremental_save(channel, len));
    ssh_processor.parallel_ssh_process(hosts);
    handler.join().unwrap();
}

fn progress_bar_creator(queue_len: u64) -> ProgressBar {
    let total_hosts_processed = ProgressBar::new(queue_len);
    let total_style = ProgressStyle::default_bar()
        .template("{eta_precise} {wide_bar} Hosts processed: {pos}/{len} Speed: {per_sec} {msg}")
        .progress_chars("##-");
    total_hosts_processed.set_style(total_style);

    total_hosts_processed
}

fn config_incremental_folders() -> File {
    let datetime = Utc::now().format("%H_%M_%S").to_string();
    let filename = &datetime;
    let store_dir_date = Utc::today().format("%d_%B_%Y").to_string();
    if !Path::new(&store_dir_date).exists() {
        std::fs::create_dir(Path::new(&store_dir_date))
            .expect("Failed creating dir for temporary save");
    }
    let incremental_name = PathBuf::from(store_dir_date + "/incremental_" + &filename + ".json");
    File::create(incremental_name).expect("incremental salving failed.")
}
enum Stat {
    Ok,
    Fail,
    TokenFail,
}
fn progress_bar_display(queue_len: u64, rx: std::sync::mpsc::Receiver<Stat>) {
    let mut ok = 0;
    let mut ko = 0;
    let mut token = 0;
    let total = progress_bar_creator(queue_len);
    for _ in 0..queue_len {
        let stat = match rx.recv() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("Error receiving stats: {}", e);
                return;
            }
        };
        match stat {
            Stat::Ok => ok += 1,
            Stat::Fail => ko += 1,
            Stat::TokenFail => token += 1,
        };
        total.inc(1);
        total.set_message(&format!("OK: {}, Failed: {}, Token: {}", ok, ko, token));
    }
}

fn incremental_save(rx: Receiver<Response>, stream_len: usize) {
    let mut file = config_incremental_folders();
    let len = stream_len;
    let (sender, reciever) = std::sync::mpsc::channel();
    std::thread::spawn(move || progress_bar_display(len as u64, reciever));
    for _ in 0..len {
        if let Ok(received) = rx.recv() {
            let stat = if received.status {
                Stat::Ok
            } else if received.result.contains("[-19]") {
                Stat::TokenFail
            } else {
                Stat::Fail
            };
            if let Err(e) = sender.send(stat) {
                eprintln!("Error sending stats: {}", e)
            }
            let mut data = serde_json::to_string_pretty(&received).unwrap();
            data += "\n";
            file.write_all(data.as_bytes())
                .expect("Writing for incremental saving failed");
        }
    }
    file.flush().expect("Failed flushing");
}
