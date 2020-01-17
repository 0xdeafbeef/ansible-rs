use serde::{Serialize, Deserialize};
use clap::{App, Arg};
use ssh2::{Session, PublicKey, Error};
use std::net::{TcpStream, ToSocketAddrs};
use std::io::{Read, BufReader};
use std::path::Path;
use std::io::prelude::*;
use std::fs::File;
use rayon::prelude::*;
use std::fmt::Display;
use rayon::ThreadPoolBuilder;
use std::time::Instant;
use humantime::format_duration;
use std::fs;
use indicatif::{ProgressBar, ProgressStyle};

#[derive(Serialize, Debug)]
struct Response {
    result: String,
    hostname: String,
    process_time: String,
    status: bool,
}

fn contruct_error<A>(hostname: &A, start_time: Instant, e: String, bar: &ProgressBar) -> Response
    where A: ToSocketAddrs + Display
{
    bar.inc(1);
    Response {
        result: e,
        hostname: hostname.to_string(),
        process_time: format_duration(Instant::now() - start_time).to_string(),
        status: false,
    }
}

fn process_host<A>(hostname: A, command: &str, bar: &ProgressBar) -> Response
    where A: ToSocketAddrs + Display {
    let start_time = Instant::now();
    let tcp = match TcpStream::connect(&hostname) {
        Ok(a) => a,
        Err(e) => {
            return contruct_error(&hostname, start_time, e.to_string(), &bar);
        }
    };
    let mut sess = match Session::new() {
        Ok(a) => a,
        Err(e) => {
            // todo logging
            return contruct_error(&hostname, start_time, e.to_string(), &bar);
        }
    };
    sess.set_tcp_stream(tcp);
    match sess.handshake() {
        Ok(a) => a,
        Err(e) => {
            return contruct_error(&hostname, start_time, e.to_string(), &bar);
        }
    };

// Try to authenticate with the first identity in the agent.
    let mut agent = match sess.agent() {
        Ok(a) => a,
        Err(e) => {
            return contruct_error(&hostname, start_time, e.to_string(), &bar);
        }
    };
    match agent.connect() {
        Ok(_) => (),
        Err(e) => {
            return contruct_error(&hostname, start_time, e.to_string(), &bar);
        }
    };
    match agent.list_identities() {
        Err(e) => return contruct_error(&hostname, start_time, e.to_string(), &bar),
        _ => {}
    };
    let id: Vec<Result<PublicKey, Error>> = agent.identities().collect();

    let key = id[0].as_ref().unwrap();
    match agent.userauth("scan", &key) {
        Ok(_) => (),
        Err(e) => return contruct_error(&hostname, start_time, e.to_string(), &bar)
    };
    let mut channel = match sess.channel_session() {
        Ok(a) => a,
        Err(e) => {
            return contruct_error(&hostname, start_time, e.to_string(), &bar);
        }
    };
    channel.exec(command).unwrap();
    let mut s = String::new();
    match channel.read_to_string(&mut s) {
        Err(e) => {
            return contruct_error(&hostname, start_time, e.to_string(), &bar);
        }
        _ => ()
    };
    let end_time = Instant::now();
    bar.inc(1);
    Response {
        hostname: hostname.to_string(),
        result: s,
        process_time: format_duration(end_time - start_time).to_string(),
        status: true,
    }
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

#[derive(Deserialize, Debug)]
struct OutputProps {
    save_to_file: bool,
    filename: Option<String>,
    pretty_format: bool,
    show_progress: bool,
}

#[derive(Deserialize, Debug)]
struct Config {
    threads: usize,
    output: OutputProps,
    command: String,
}

impl Default for OutputProps {
    fn default() -> Self {
        OutputProps {
            save_to_file: false,
            filename: None,
            pretty_format: false,
            show_progress: false,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            threads: 10,
            command: String::default(),
            output: OutputProps::default(),
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

fn save_to_file(conf: &Config, data: Vec<Response>)
{
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
    match conf.output.pretty_format {
        true => {
            match serde_json::to_writer_pretty(file, &data) {
                Ok(_) => println!("Saved successfully"),
                Err(e) => eprintln!("Error saving: {}", e)
            };
        }
        false => {
            match serde_json::to_writer(file, &data) {
                Ok(_) => println!("Saved successfully"),
                Err(e) => eprintln!("Error saving: {}", e)
            }
        }
    }
}

fn save_to_console(conf: &Config, data: &Vec<Response>) {
    match conf.output.pretty_format {
        true => println!("{}", serde_json::to_string_pretty(&data).unwrap()),
        false => println!("{}", serde_json::to_string(&data).unwrap())
    }
}

fn main() {
    let args = App::new("SSH analyzer")
        .arg(
            Arg::with_name("config")
                .short("c")
                .long("config")
                .help("Path to hosts file")
                .required(false)
                .takes_value(true)
                .default_value("config.toml")
        )
        .arg(Arg::with_name("hosts")
            .long("hosts")
            .help("Path to file with hosts")
            .required(true)
            .takes_value(true))
        .get_matches();
    let hosts = hosts_builder(Path::new(&args.value_of("hosts").unwrap()));
    let config = get_config(Path::new(&args.value_of("config").unwrap()));
    dbg!(&config);
    let pool = ThreadPoolBuilder::new().num_threads(config.threads).build().expect("failed creating pool");
    let command = &config.command;
    let element_nums = hosts.len() as u64;
    let style = ProgressStyle::default_bar()
        .template("{eta} {wide_bar:80.yellow} Hosts processed: {pos}/{len}")
        .progress_chars("##-");
    let pb = ProgressBar::new(element_nums);
    pb.set_style(style.clone());
    pb.tick();
    let result: Vec<Response> = pool.install(
        || hosts.par_iter()
            .map(|x| process_host(x, &command, &pb))
            .collect());
    pb.finish_and_clear();
    if config.output.save_to_file {
        save_to_file(&config, result);
    } else {
        save_to_console(&config, &result);
    }
}
