use ansible_rs::Response;
use ansible_rs::{ParallelSshProps, ParallelSshPropsBuilder};
use chrono::Utc;
use clap::crate_version;
use clap::{App, Arg};
use futures::{
    AsyncWriteExt,
    Future,
    StreamExt
};
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};

use tracing_subscriber;

use std::fs::File;
use std::io::prelude::*;
use std::io::{BufReader};
use std::path::{Path, PathBuf};

use async_channel::Receiver;
use async_executor::{Executor, Spawner, Task};
use std::pin::Pin;
use std::time::Duration;
use tokio_trace::level_filters::LevelFilter;

fn hosts_builder(path: &Path) -> Vec<String> {
    let file = File::open(path).expect("Unable to open the file");
    let reader = BufReader::new(file);
    reader
        .lines()
        .map(|l| l.unwrap() + ":22")
        .collect::<Vec<String>>()
}

#[derive(Deserialize, Debug, Clone, Serialize)]
struct OutputProps {
    save_to_file: bool,
    filename: Option<String>,
    pretty_format: bool,
    show_progress: bool,
    keep_incremental_data: Option<bool>,
}

#[derive(Deserialize, Debug, Clone, Serialize)]
struct Config {
    threads: usize,
    agent_parallelism: usize,
    command: String,
    timeout: u64,
    connection_timeout: u64,
    output: OutputProps,
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
            agent_parallelism: 2,
            command: String::default(),
            output: OutputProps::default(),
            timeout: 60,
            connection_timeout: 100,
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
            eprintln!("Error saving content to file:{}", e);
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

fn save_to_console(conf: &Config, data: &[Response]) {
    if conf.output.pretty_format {
        println!("{}", serde_json::to_string_pretty(&data).unwrap())
    } else {
        println!("{}", serde_json::to_string(&data).unwrap())
    }
}

fn main() {
    color_backtrace::install();
    // tracing_subscriber::fmt()
    //     .with_max_level(tracing::Level::TRACE)
    //     .init();
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
    let config: Config = confy::load_path("./confy_new.toml").unwrap();
    let hosts = hosts_builder(Path::new(&args.value_of("hosts").unwrap()));
    dbg!(&config);

    let (rx, processor) = ParallelSshPropsBuilder::default()
        .maximum_connections(config.threads)
        .agent_parallelism(config.agent_parallelism)
        .timeout_socket(Duration::from_millis(config.connection_timeout))
        .timeout_ssh(Duration::from_secs(config.timeout))
        .build()
        .expect("Failed building ssh processor properties");
    dbg!(&processor);
    let com = config.command;
    let hosts_stream = processor.parallel_ssh_process(hosts, &com);
    incremental_save(hosts_stream, rx);
    // match config.output.keep_incremental_data {
    //     Some(true) => {}
    //     Some(false) => {
    //         match std::fs::remove_file(Path::new(&inc_for_closure)) {
    //             Ok(_) => (),
    //             Err(e) => eprintln!("Error removing temp file : {}", e),
    //         };
    //     }
    //     None => {
    //         match std::fs::remove_file(Path::new(&inc_for_closure)) {
    //             Ok(_) => (),
    //             Err(e) => eprintln!("Error removing temp file : {}", e),
    //         };
    //     }
    // };
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

fn incremental_save(
    stream: Vec<Pin<Box<dyn Future<Output = ()> + std::marker::Send>>>,
    rx: Receiver<Response>,
) {
    // println!("here");
    let mut file = blocking::Unblock::new(config_incremental_folders());
    let total = progress_bar_creator(stream.len() as u64);
    let len = stream.len();
    smol::run(async {
        for fut in stream {
            Task::spawn(fut).detach();
        }
        let mut ok: i32 = 0;
        let mut ko: i32 = 0;
        let mut token_fail: i32 = 0;
        file.write_all(b"[\r\n")
            .await
            .expect("Writing for incremental saving failed");
        for _ in 0..len {
            if let Ok(received) = rx.recv().await {
                if received.status {
                    ok += 1
                } else {
                    ko += 1;
                    if received.result.contains("[-19]") {
                        token_fail += 1;
                    }
                };
                total.inc(1);
                total.set_message(&format!(
                    "OK: {}, Failed: {}, Token: {}",
                    ok, ko, token_fail
                ));
                let mut data = serde_json::to_string_pretty(&received).unwrap();
                data += ",\n";
                file.write_all(data.as_bytes())
                    .await
                    .expect("Writing for incremental saving failed");
            }
        }
        file.write_all(b"\n]")
            .await
            .expect("Writing for incremental saving failed");
    });
}
