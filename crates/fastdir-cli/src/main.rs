//! `fastdir-cli`: the console build of FastDir's command line. Unlike the GUI
//! `fastdir` executable it is a console program on every platform, so shells
//! wait for it and its output always reaches the terminal.

fn main() {
    let args = std::env::args_os().skip(1).collect();
    std::process::exit(fastdir_cli::run(args));
}
