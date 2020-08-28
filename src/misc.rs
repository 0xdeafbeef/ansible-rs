
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct OutputProps {
    pub save_to_file: bool,
    pub filename: String,
    pub pretty_format: bool,
    pub show_progress: bool,
    pub keep_incremental_data: Option<bool>,
    pub root_module_path: Option<PathBuf>,
}

#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct Config {
    pub threads: usize,
    pub agent_parallelism: isize,
    pub command: String,
    pub timeout: u32,
    pub modules_root: Option<PathBuf>,
    pub output: OutputProps, //should be last field
}

impl Default for OutputProps {
    fn default() -> Self {
        OutputProps {
            save_to_file: false,
            filename: String::default(),
            pretty_format: false,
            show_progress: false,
            keep_incremental_data: Some(false),
            root_module_path: Option::default(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            threads: 10,
            agent_parallelism: 1,
            command: "uptime".to_string(),
            output: OutputProps::default(),
            timeout: 60,
            modules_root: Some(PathBuf::from("./modules")),
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
    path: &Path,
) -> Result<BTreeMap<Ipv4Addr, String>, std::io::Error> {
    let mut rd = csv::ReaderBuilder::new().from_path(path)?;
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
//
// pub fn get_config(path: &Path) -> Config {
//     let f = match fs::read_to_string(path) {
//         Ok(a) => a,
//         Err(e) => {
//             eprintln!("Failed reading config. Using default values : {}", e);
//             return Config::default();
//         }
//     };
//     match toml::from_str(f.as_str()) {
//         Ok(t) => t,
//         Err(e) => {
//             eprintln!("Error parsing config:{}", e);
//             Config::default()
//         }
//     }
// }
//
// pub fn save_to_file(conf: &Config, data: Vec<Response>) {
//     let filename = match &conf.output.filename {
//         None => {
//             eprintln!("Filename to save is not given. Printing to stdout.");
//             save_to_console(&conf, &data);
//             return;
//         }
//         Some(a) => Path::new(a.as_str()),
//     };
//
//     let file = match File::create(filename) {
//         Ok(a) => a,
//         Err(e) => {
//             eprintln!("Erorr saving content to file:{}", e);
//             save_to_console(&conf, &data);
//             return;
//         }
//     };
//     if conf.output.pretty_format {
//         match serde_json::to_writer_pretty(file, &data) {
//             Ok(_) => println!("Saved successfully"),
//             Err(e) => eprintln!("Error saving: {}", e),
//         };
//     } else {
//         match serde_json::to_writer(file, &data) {
//             Ok(_) => println!("Saved successfully"),
//             Err(e) => eprintln!("Error saving: {}", e),
//         }
//     }
// }
//
// pub fn save_to_console(conf: &Config, data: &Vec<Response>) {
//     if conf.output.pretty_format {
//         println!("{}", serde_json::to_string_pretty(&data).unwrap())
//     } else {
//         println!("{}", serde_json::to_string(&data).unwrap())
//     }
// }
