#[macro_use]
extern crate clap;

use fern;
use std::process;

mod bash;
mod cache;
mod cmds;
mod config;
mod env;
mod error;
mod status;
mod sums;

fn main() {
    if let Err(err) = init() {
        eprintln!("{}", err);
        process::exit(2);
    };

    let matches = clap::App::new("firstaide")
        .version(crate_version!())
        .author(crate_authors!())
        .about("First, as in prior to, aide.")
        .subcommand(cmds::build::argspec())
        .subcommand(cmds::status::argspec())
        .subcommand(cmds::clean::argspec())
        .subcommand(cmds::hook::argspec())
        .subcommand(cmds::env::argspec().setting(clap::AppSettings::Hidden))
        .setting(clap::AppSettings::DeriveDisplayOrder)
        .setting(clap::AppSettings::SubcommandRequired)
        .get_matches();

    use error::Error::*;
    let result: Result<u8, error::Error> = match matches.subcommand() {
        (cmds::build::NAME, Some(subm)) => cmds::build::run(subm).map_err(BuildError),
        (cmds::status::NAME, Some(subm)) => cmds::status::run(subm).map_err(StatusError),
        (cmds::clean::NAME, Some(subm)) => cmds::clean::run(subm).map_err(CleanError),
        (cmds::hook::NAME, Some(subm)) => cmds::hook::run(subm).map_err(HookError),
        (cmds::env::NAME, Some(subm)) => cmds::env::run(subm).map_err(EnvError),
        // This last branch should not be taken while `SubcommandRequired` is in
        // effect, but Rust insists that we cater for it, so we do.
        (name, _) => Err(CommandNotFound(name.into())),
    };

    match result {
        Err(err) => {
            log::error!("{}", err);
            process::exit(2);
        }
        Ok(code) => {
            process::exit(code as i32);
        }
    };
}

fn init() -> Result<(), log::SetLoggerError> {
    fern::Dispatch::new()
        // Perform allocation-free log formatting.
        .format(|out, message, record| {
            out.finish(format_args!(
                "{}  {}  {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                // record.target(),
                record.level(),
                message
            ))
        })
        // Add blanket level filter.
        .level(log::LevelFilter::Debug)
        // Output to stdout.
        .chain(std::io::stdout())
        // Apply globally.
        .apply()
}
