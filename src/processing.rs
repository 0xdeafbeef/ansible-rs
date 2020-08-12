// use pyo3::{
//     prelude::*,
//     types::{IntoPyDict, PyModule},
// };
// use std::io::Read;
// use std::path::PathBuf;
//
// pub struct Processing<'a> {
//     module: &'a PyModule
// }
//
// impl <'a>Processing<'a> {
//     fn new(file_path:  PathBuf) -> std::io::Result<Self> {
//         let mut file = std::fs::File::open(&file_path)?;
//         let mut code = String::new();
//         file.read_to_string(&mut code)?;
//         let gil = Python::acquire_gil();
//         let py = gil.python();
//         let module = PyModule::from_code(py.clone(), &code, &file_path.to_string_lossy(), "processing")?;
//         Ok(Processing { module })
//     }
// }
