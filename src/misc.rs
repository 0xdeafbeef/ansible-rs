use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use indicatif::{ProgressBar, ProgressStyle};
use std::sync::mpsc::Receiver;
use chrono::Utc;

#[derive(Serialize, Debug, Clone)]
pub struct Response {
    pub result: String,
    pub hostname: String,
    pub process_time: String,
    pub status: bool,
}

#[derive(Deserialize, Debug, Clone)]
pub struct OutputProps {
    pub save_to_file: bool,
    pub filename: Option<String>,
    pub pretty_format: bool,
    pub show_progress: bool,
    pub keep_incremental_data: Option<bool>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Config {
    pub threads: usize,
    pub agent_parallelism: isize,
    pub output: OutputProps,
    pub command: String,
    pub timeout: u32,
    pub modules_path: Option<String>,
}
#[derive(Deserialize, Debug, Clone)]
pub struct ModulesParams{
    path  :String
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
            agent_parallelism: 1,
            command: String::default(),
            output: OutputProps::default(),
            timeout: 60,
            modules_path: Some("modules".to_string())
        }
    }
}

pub fn hosts_builder(path: &Path) -> Vec<Ipv4Addr> {
    let file = File::open(path).expect("Unable to open the file");
    let reader = BufReader::new(file);
    reader
        .lines()
        .map(|l| l.unwrap_or("Error reading line".to_string()))
        .map(|l| l.replace("\"", ""))
        .map(|l| l.replace("'", ""))
        .map(|l| l.parse())
        .filter_map(Result::ok)
        .collect()
}

pub fn generate_kv_hosts_from_csv(
    path: &str,
) -> Result<BTreeMap<Ipv4Addr, String>, std::io::Error> {
    let mut rd = csv::ReaderBuilder::new().from_path(Path::new(path))?;
    let mut map = BTreeMap::new();
    for res in rd.records() {
        let rec = match res {
            Ok(a) => a,
            Err(_) => continue,
        };
        let k: Ipv4Addr = match rec.get(0).unwrap().parse() {
            Ok(a) => a,
            Err(_) => continue,
        };
        let v = rec.get(1).unwrap();
        println!("{} {}", &k, &v);
        map.insert(k, v.to_string());
    }
    Ok(map)
}

pub fn get_config(path: &Path) -> Config {
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

pub fn save_to_file(conf: &Config, data: Vec<Response>) {
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

pub fn save_to_console(conf: &Config, data: &Vec<Response>) {
    if conf.output.pretty_format {
        println!("{}", serde_json::to_string_pretty(&data).unwrap())
    } else {
        println!("{}", serde_json::to_string(&data).unwrap())
    }
}
fn progress_bar_creator(queue_len: u64) -> ProgressBar {
    let total_hosts_processed = ProgressBar::new(queue_len);
    let total_style = ProgressStyle::default_bar()
        .template("{eta_precise} {wide_bar} Hosts processed: {pos}/{len} Speed: {per_sec} {msg}")
        .progress_chars("##-");
    total_hosts_processed.set_style(total_style);

    total_hosts_processed
}

pub fn incremental_save(rx: Receiver<Response>, props: &OutputProps, queue_len: u64, filename: &str) {
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
