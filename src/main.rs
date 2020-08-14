use std::collections::BTreeMap;
use std::fmt::Display;
use std::fs;
use std::fs::File;
use std::io::prelude::*;
use std::io::Read;
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{mpsc, Arc};
use std::thread::spawn;
use std::time::Instant;
mod misc;
use chrono::Utc;
use clap::crate_version;
use clap::{App, Arg};
use color_backtrace;
use humantime::format_duration;
use indicatif::{ProgressBar, ProgressStyle};
use misc::*;
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;

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
