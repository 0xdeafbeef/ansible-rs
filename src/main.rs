use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{mpsc, Arc};
use std::thread::spawn;
mod misc;
use chrono::Utc;
use clap::crate_version;
use clap::{App, Arg};
use color_backtrace;
use misc::*;
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use std_semaphore::Semaphore;
mod host_processing;
use host_processing::*;

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

