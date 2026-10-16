mod cli;
mod config;
mod control;
mod daemon;
mod dns;
mod fastcgi;
mod fpm;
mod herd;
mod install;
mod paths;
mod php;
mod proxy;
mod server;
mod shim;
mod sites;
mod tls;
mod ui;

fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let mut args = std::env::args_os();
    let argv0 = args.next().unwrap_or_default();
    if let Some(shim) = shim::detect(&argv0.to_string_lossy()) {
        if let Err(e) = shim::run(shim, args.collect()) {
            eprintln!("bird: {e:#}");
            std::process::exit(127);
        }
        return;
    }

    if let Err(e) = cli::main() {
        eprintln!("{} {e:#}", ui::red("error:"));
        std::process::exit(1);
    }
}
