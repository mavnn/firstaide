use crate::cache;
use crate::config;
use crate::env;
use crate::status::EnvironmentStatus;
use crate::sums;
use bstr::ByteSlice;
use std::env::vars_os;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use tempfile;

pub const NAME: &str = "hook";

type Result = std::result::Result<u8, Error>;

pub enum Error {
    Config(config::Error),
    Io(io::Error),
    EnvOutsideCapture,
    EnvOutsideDecode(bincode::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use Error::*;
        match self {
            Config(err) => write!(f, "{}", err),
            Io(err) => write!(f, "input/output error: {}", err),
            EnvOutsideCapture => write!(f, "could not capture outside environment"),
            EnvOutsideDecode(err) => write!(f, "problem decoding outside environment: {}", err),
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
        .about("Hooks the development environment; source the output from .envrc")
        .arg(
            clap::Arg::with_name("dir")
                .value_name("DIR")
                .help("The directory in which to build"),
        )
}

pub fn run(args: &clap::ArgMatches) -> Result {
    let config = config::Config::load(args.value_of_os("dir"))?;
    let stdout = io::stdout();
    let mut handle = stdout.lock();

    // Wrap everything in { ... } so that it's only evaluated by Bash once
    // completely written out. This is for correctness, but it might also help
    // prevent seeing broken pipe errors.
    writeln!(&mut handle, "{{ # Start.")?;
    writeln!(&mut handle)?;

    fn chunk(title: &str, chunk: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        let comments = title.lines().map(|line| format!("### {}\n", line));
        buf.extend(comments.map(String::into_bytes).flatten());
        buf.extend(chunk);
        buf.push(b'\n');
        buf
    }

    fn watch<T: Into<OsString>>(filename: T) -> Vec<u8> {
        let function: &[u8] = b"watch_file";
        let filename = crate::bash::escape(filename);
        let mut out = Vec::with_capacity(function.len() + 1 + filename.len() + 1);
        out.extend(function);
        out.push(b' ');
        out.extend(filename);
        out.push(b'\n');
        out
    }

    // Setting up additional OS pipes for subprocesses to communicate back to us
    // is not well supported in the Rust standard library, so we use files in a
    // temporary directory instead.
    let temp_dir = tempfile::TempDir::new_in(&config.cache_dir)?;
    let temp_path = temp_dir.path().to_owned();

    let env_outside: env::Env = {
        let dump_path = temp_path.join("outside");
        let mut dump_cmd = config.command_to_dump_env_outside(&dump_path);
        let mut dump_proc = dump_cmd.spawn()?;
        if !dump_proc.wait()?.success() {
            return Err(Error::EnvOutsideCapture);
        }
        match bincode::deserialize(&fs::read(dump_path)?) {
            Ok(env) => Ok(env),
            Err(err) => Err(Error::EnvOutsideDecode(err)),
        }
    }?;

    let env: env::Env = vars_os().collect();

    let mut diff = env::diff(&env, &env_outside).exclude_by_prefix(b"DIRENV_");

    let watches = env_outside.iter().find(|(key, _)| key == "DIRENV_WATCHES");

    if let Some((key, value)) = watches {
        diff.push(env::Added(key.clone(), value.clone()));
    }

    handle.write_all(&chunk(
        "Parent environment follows:",
        &env_diff_dump(&diff),
    ))?;

    handle.write_all(&chunk("Helpers.", include_bytes!("hook/helpers.sh")))?;

    match cache::Cache::load(config.cache_file()) {
        Ok(cache) => {
            let sums_now = sums::Checksums::from(&config.watch_files()?)?;
            if sums::equal(&sums_now, &cache.sums) {
                let chunk_message = crate::bash::escape(&config.messages.getting_started);
                let chunk_content =
                    include_bytes!("hook/active.sh").replace(b"__MESSAGE__", chunk_message);
                handle.write_all(&chunk(&EnvironmentStatus::Okay.display(), &chunk_content))?;
                handle.write_all(&chunk(
                    "Cached environment follows:",
                    &env_diff_dump(&cache.diff),
                ))?;
            } else {
                handle.write_all(&chunk(
                    &EnvironmentStatus::Stale.display(),
                    include_bytes!("hook/stale.sh"),
                ))?;
                handle.write_all(&chunk(
                    "Cached environment follows:",
                    &env_diff_dump(&cache.diff),
                ))?;
            }
            let watches = cache.sums.into_iter().map(|sum| watch(sum.path()));
            handle.write_all(&chunk(
                "Watch dependencies.",
                &watches.flatten().collect::<Vec<u8>>(),
            ))?;
        }
        Err(_) => {
            handle.write_all(&chunk(
                &EnvironmentStatus::Unknown.display(),
                include_bytes!("hook/inactive.sh"),
            ))?;
        }
    };

    handle.write_all(&chunk("Watch the cache file.", &watch(config.cache_file())))?;

    writeln!(&mut handle, "}} # End.")?;

    Ok(0)
}

pub fn env_diff_dump(diff: &env::Diff) -> Vec<u8> {
    use crate::bash::escape as esc;
    use crate::env::Change::*;

    // Filter out DIRENV_ and SSH_ vars.
    let diff = diff
        .exclude_by_prefix(b"DIRENV_")
        .exclude_by_prefix(b"SSH_");

    let mut output: Vec<u8> = Vec::new();
    for change in &diff {
        match change {
            Added(k, vb) => {
                output.extend(b"export ");
                output.extend(esc(k));
                output.extend(b"=");
                output.extend(esc(vb));
            }
            Changed(k, _va, vb) => {
                output.extend(b"export ");
                output.extend(esc(k));
                output.extend(b"=");
                output.extend(esc(vb));
            }
            Removed(k, _va) => {
                output.extend(b"unset ");
                output.extend(esc(k));
            }
        }
        output.push(b'\n');
    }
    output
}
