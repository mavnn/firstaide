use crate::cache;
use crate::config;
use crate::env;
use crate::sums;
use bincode;
use spinners::{Spinner, Spinners};
use std::fmt;
use std::fs;
use std::io;
use tempfile;

pub const NAME: &str = "build";

type Result = std::result::Result<u8, Error>;

pub enum Error {
    Config(config::Error),
    Io(io::Error),
    DirEnv(String),
    EnvOutsideCapture,
    EnvOutsideDecode(bincode::Error),
    EnvInsideCapture,
    EnvInsideDecode(bincode::Error),
    Cache(bincode::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use Error::*;
        match self {
            Config(err) => write!(f, "{}", err),
            Io(err) => write!(f, "input/output error: {}", err),
            DirEnv(message) => write!(f, "direnv broke: {}", message),
            EnvOutsideCapture => write!(f, "could not capture outside environment"),
            EnvOutsideDecode(err) => write!(f, "problem decoding outside environment: {}", err),
            EnvInsideCapture => write!(f, "could not capture inside environment"),
            EnvInsideDecode(err) => write!(f, "problem decoding inside environment: {}", err),
            Cache(err) => write!(f, "cache could not be saved: {}", err),
        }
    }
}

impl From<config::Error> for Error {
    fn from(error: config::Error) -> Self {
        Error::Config(error)
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Error::Io(error)
    }
}

pub fn argspec<'a, 'b>() -> clap::App<'a, 'b> {
    clap::SubCommand::with_name(NAME)
        .about("Builds the development environment and captures its environment variables")
        .arg(
            clap::Arg::with_name("dir")
                .value_name("DIR")
                .help("The directory in which to build"),
        )
}

pub fn run(args: &clap::ArgMatches) -> Result {
    let config = config::Config::load(args.value_of_os("dir"))?;
    build(config)
}

fn spin<F, T>(f: F) -> T
where
    F: FnOnce() -> T,
{
    if atty::is(atty::Stream::Stdout) {
        let spinner = Spinner::new(Spinners::Dots, "".into());
        let result = f();
        spinner.stop();
        print!("\x08\x08"); // Backspace over the spinner.
        result
    } else {
        f()
    }
}

fn build(config: config::Config) -> Result {
    // 0. Check `direnv` is new enough. Older versions have bugs that prevent
    // building from working correctly.
    check_direnv_version(&config).map_err(Error::DirEnv)?;

    // 1. Allow `direnv`.
    log::info!("Allow direnv in {:?}.", &config.build_dir);
    if !config.command_to_allow_direnv().status()?.success() {
        return Err(Error::DirEnv("could not enable direnv".into()));
    }

    // 2. Create output directory.
    log::info!("Create cache dir at {:?}.", &config.cache_dir);
    fs::create_dir_all(&config.cache_dir)?;

    // Setting up additional OS pipes for subprocesses to communicate back to us
    // is not well supported in the Rust standard library, so we use files in a
    // temporary directory instead.
    let temp_dir = tempfile::TempDir::new_in(&config.cache_dir)?;
    let temp_path = temp_dir.path().to_owned();

    // 3a. Capture outside environment.
    log::info!("Capture outside environment.");
    let env_outside: env::Env = spin(|| {
        let dump_path = temp_path.join("outside");
        let mut dump_cmd = config.command_to_dump_env_outside(&dump_path);
        log::debug!("{:?}", dump_cmd);
        let mut dump_proc = dump_cmd.spawn()?;
        if !dump_proc.wait()?.success() {
            return Err(Error::EnvOutsideCapture);
        }
        match bincode::deserialize(&fs::read(dump_path)?) {
            Ok(env) => Ok(env),
            Err(err) => Err(Error::EnvOutsideDecode(err)),
        }
    })?;

    // 3b. Capture inside environment.
    log::info!("Capture inside environment (may involve a full build).");
    let env_inside: env::Env = spin(|| {
        let dump_path = temp_path.join("inside");
        let mut dump_cmd = config.command_to_dump_env_inside(&dump_path, &env_outside);
        log::debug!("{:?}", dump_cmd);
        let mut dump_proc = dump_cmd.spawn()?;
        if !dump_proc.wait()?.success() {
            return Err(Error::EnvInsideCapture);
        }
        match bincode::deserialize(&fs::read(dump_path)?) {
            Ok(env) => Ok(env),
            Err(err) => Err(Error::EnvInsideDecode(err)),
        }
    })?;

    // We're done with the temporary directory.
    drop(temp_path);
    drop(temp_dir);

    // 4. Calculate environment diff.
    log::info!("Calculate environment diff.");
    let env_diff = env::diff(&env_outside, &env_inside);

    // 5. Calculate checksums.
    log::info!("Calculate file checksums.");
    let checksums = spin(|| sums::Checksums::from(&config.watch_files()?))?;

    // 6. Write out cache.
    log::info!("Write out cache.");
    let cache = cache::Cache {
        diff: env_diff,
        sums: checksums,
    };
    cache.save(config.cache_file()).map_err(Error::Cache)?;

    // Done.
    Ok(0)
}

fn check_direnv_version(config: &config::Config) -> std::result::Result<(), String> {
    let version_min = semver::Version::new(2, 20, 1);
    let mut command = config.command_direnv();
    command.arg("version");
    let command_output = command.output().map_err(|err| format!("{}", err))?;
    let version_string = String::from_utf8_lossy(&command_output.stdout);
    let version = semver::Version::parse(&version_string)
        .map_err(|err| format!("could not parse version {:?}: {}", version_string, err))?;
    if version < version_min {
        Err(format!(
            "direnv is too old ({}); upgrade to {} or later (hint: use `nix-env -i direnv`)",
            version, version_min,
        ))
    } else {
        Ok(())
    }
}
