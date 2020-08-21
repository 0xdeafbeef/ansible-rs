use anyhow::Error;
use serde::{Deserialize, Deserializer};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use toml::from_str;
use walkdir::{DirEntry, WalkDir};

enum ExecType {
    Bin,
    Python,
    Bash,
}
impl From<&str> for ExecType {
    fn from(a: &str) -> Self {
        match a.to_lowercase().as_str() {
            "bin" => Self::Bin,
            "bash" => Self::Bash,
            "sh" => Self::Bash,
            "py" => Self::Python,
            "python" => Self::Python,
            _ => panic!(format!("Bad module type is provided: {}", a)),
        }
    }
}

impl<'de> Deserialize<'de> for ExecType {
    fn deserialize<D>(deserializer: D) -> Result<Self, <D as Deserializer<'de>>::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(ExecType::from(s.as_str()))
    }
}

#[derive(Deserialize)]
pub struct ModuleProps {
    module_name: String,
    module_type: ExecType,
    exec_path: PathBuf,
}
impl ModuleProps {
  pub  fn new(path: &Path) -> Result<ModuleProps, Error> {
        let mut file = File::open(path)?;
        let mut content = String::new();
        file.read_to_string(&mut content)?;
        let res = from_str(&content)?;
        Ok(res)
    }
    fn check_filename(filename: &DirEntry) -> bool {
        let ext = match filename.path().extension() {
            Some(a) => a.to_string_lossy().to_lowercase(),
            None => return false,
        };
        if ext == "toml" {
            true
        } else {
            false
        }
    }
}
pub struct ModuleTree {
    Tree: HashMap<String, ModuleProps>,
}

impl ModuleTree {
    pub fn new(path: &Path) -> Self {
        let root = WalkDir::new(path);
        let map: HashMap<_, _> = root
            .into_iter()
            .filter_entry(|e| ModuleProps::check_filename(e))
            .filter_map(|e| e.ok())
            .map(|name| (ModuleProps::new(name.path()), name))
            .filter_map(|(x, name)| {
                if let Err(e) = x {
                    eprintln!("Error reading config: {}", e);
                    None
                } else {
                    Some((name.path().to_string_lossy().to_string(), x.unwrap()))
                }
            })
            .collect();
        ModuleTree { Tree: map }
    }
    pub fn run_module(&self, module_name: &str, ) ->Result<(), Error>
    {
        
    }
}
