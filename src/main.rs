use clap::{App, Arg};
use ssh2::Session;


fn main() {
    let args = App::new("SSH analyzer")
        .arg(
            Arg::with_name("config")
                .short("c")
                .long("config")
                .help("Path to hosts file"),
        )
        .arg(
            Arg::with_name("type")
                .short("t")
                .long("type")
                .help("Type of config. Default is host. Possible values: host, ip")
                .default_value("host")
                .case_insensitive(true),
        ).get_matches();
    println!("{:#?}", args);

    // Almost all APIs require a `Session` to be available
    let sess = Session::new().unwrap();
    let mut agent = sess.agent().unwrap();

    // Connect the agent and request a list of identities
    agent.connect().unwrap();
    agent.list_identities().unwrap();
    Session::new()

}
