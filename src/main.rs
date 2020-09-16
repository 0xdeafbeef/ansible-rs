use ansible_rs::ssh::{ParallelSshPropsBuilder, Response};
use chrono::Utc;

use color_backtrace;
use crossbeam_channel::Receiver;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::ThreadPoolBuilder;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::prelude::*;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use std::thread::spawn;
use std::time::Duration;

mod misc;
use ansible_modules::ModuleTree;
use misc::{generate_kv_hosts_from_csv, hosts_builder, Config};
use std::net::SocketAddr;
use structopt::StructOpt;

#[derive(StructOpt, Debug)]
#[structopt(name = "ansible-rs")]
struct Args {
    #[structopt(long)]
    #[structopt(short, help = "Path to config", default_value = "./config.toml")]
    config: String,
    #[structopt(long, help = "Path to hosts file")]
    hosts: PathBuf,
    #[structopt(long, default_value = "list")]
    hosts_format: String,
    #[structopt(long, short, help = "Module name")]
    module: Option<Vec<String>>,
}

fn main() {
    enum Hosts {
        Kv(BTreeMap<SocketAddr, String>),
        Linear(Vec<SocketAddr>),
    }
    color_backtrace::install();
    let args: Args = Args::from_args();
    let config: Config = confy::load_path(args.config).unwrap();
    let command = &config.command;
    let hosts = if args.module.is_none() {
        Hosts::Kv(if args.hosts_format == "csv" {
            let hosts = generate_kv_hosts_from_csv(&args.hosts).unwrap();
            hosts
                .into_iter()
                .map(|(ad, com)| (SocketAddr::new(IpAddr::from(ad), 22), com))
                .collect()
        } else {
            let mut map = BTreeMap::new();
            for h in hosts_builder(&args.hosts) {
                map.insert(SocketAddr::new(IpAddr::from(h), 22), command.clone());
            }
            map
        })
    } else {
        Hosts::Linear(
            hosts_builder(&args.hosts)
                .into_iter()
                .map(|h| SocketAddr::new(IpAddr::from(h), 22))
                .collect(),
        )
    };
    let len = match &hosts {
        Hosts::Kv(a) => a.len(),
        Hosts::Linear(a) => a.len(),
    };
    let save_filename = config.output.filename.clone();

    dbg!(&config);
    ThreadPoolBuilder::new()
        .num_threads(config.threads)
        .build_global()
        .expect("failed creating pool");

    let mut builder = ParallelSshPropsBuilder::default();
    builder
        .agent_connections_pool(config.agent_parallelism)
        .tcp_connections_pool(config.threads as isize)
        .timeout_socket(Duration::from_millis(config.timeout as u64))
        .timeout_ssh(Duration::from_secs(60));
    let (channel, ssh_processor) = match args.module {
        Some(_) => builder.set_module_tree(ModuleTree::new(
            &config
                .modules_root
                .expect("Module flag set, but there is no `modules_root` in config"),
        )),
        None => &mut builder,
    }
    .build()
    .expect("Failed building ssh_processor instance");

    let handler = spawn(move || incremental_save(channel, len, &save_filename));
    match hosts {
        Hosts::Linear(hosts) => {
            if let None =
                ssh_processor.parallel_module_evaluation(hosts, args.module.unwrap()[0].clone())
            {
                return;
            }
        }
        Hosts::Kv(hosts) => ssh_processor.parallel_command_evaluation(hosts),
    };
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

fn config_incremental_folders() -> (File, PathBuf) {
    let datetime = Utc::now().format("%H_%M_%S").to_string();
    let filename = &datetime;
    let store_dir_date = Utc::today().format("%d_%B_%Y").to_string();
    if !Path::new(&store_dir_date).exists() {
        std::fs::create_dir(Path::new(&store_dir_date))
            .expect("Failed creating dir for temporary save");
    }
    let incremental_name = PathBuf::from(store_dir_date + "/incremental_" + &filename + ".json");
    (
        File::create(&incremental_name).expect("incremental salving failed."),
        incremental_name,
    )
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

fn create_tmp_folder_for_token_failed_hosts(name: &str) -> std::io::Result<File> {
    File::create(&name)
}

fn incremental_save(rx: Receiver<Response>, stream_len: usize, config_filename: &str) {
    let (mut file, name) = config_incremental_folders();
    let len = stream_len;
    let (sender, reciever) = std::sync::mpsc::channel();
    std::thread::spawn(move || progress_bar_display(len as u64, reciever));
    let temp_name = name.to_string_lossy().to_string() + "token_failed.txt";
    let mut failed_token_dump = create_tmp_folder_for_token_failed_hosts(&temp_name)
        .expect("Failed creating file for hosts failed with token");
    let mut token_failed = false;
    for _ in 0..len {
        if let Ok(received) = rx.recv() {
            let stat = if received.status {
                Stat::Ok
            } else if received.result.contains("[-19]") {
                token_failed = true;
                failed_token_dump
                    .write_all((&received.hostname).as_ref())
                    .expect("Failed writing name of failed file");
                failed_token_dump
                    .write_all(b"\r\n")
                    .expect("Failed writing name of failed file");

                Stat::TokenFail
            } else {
                Stat::Fail
            };
            if let Err(e) = sender.send(stat) {
                eprintln!("Error sending stats: {}", e)
            }
            let mut data = serde_json::to_string(&received).unwrap();
            data += "\n";
            file.write_all(data.as_bytes())
                .expect("Writing for incremental saving failed");
        }
    }
    file.flush().expect("Failed flushing");
    std::fs::rename(&name, &config_filename)
        .expect("Failed moving temp file to location specified in the config :(");
    if !token_failed {
        std::fs::remove_file(&temp_name).expect("Failed removing temp file");
    }
}
