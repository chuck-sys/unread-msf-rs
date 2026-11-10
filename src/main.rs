use std::io::{self, BufRead, BufReader};
use std::fs::{self, File};
use std::path::{PathBuf, Path};
use std::str::FromStr;
use std::process::{Stdio, Command};
use std::env::Args;

mod msf;
mod inotify;

const BUF_SIZE: usize = 64 * 1024;
const IPC_DEFAULT_FILENAME: &'static str = "unread-msf";
const USAGE: &'static str = "
usage: unread-msf   [--output FILE]
                    [--on-change CMD]
                    file1.msf file2.msf ...
";

struct Options {
    ipc_filename: PathBuf,
    hook: Option<String>,
    watched_paths: Vec<PathBuf>,
}

impl Options {
    fn from_args(args: Args) -> Result<Self, i32> {
        let mut hook = None;
        let mut ipc_filename = PathBuf::from_str(IPC_DEFAULT_FILENAME).unwrap();
        let mut watched_paths = Vec::with_capacity(args.len() - 1);
        let mut it = args.skip(1);
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--output" | "--on-change" => {
                    let Some(val) = it.next() else {
                        eprintln!("{arg} needs a value\n{USAGE}");
                        return Err(1);
                    };

                    if arg == "--on-change" {
                        hook = Some(val.to_string());
                    } else {
                        ipc_filename = PathBuf::from_str(&val).unwrap();
                    }
                }
                _ => watched_paths.push(PathBuf::from_str(&arg).unwrap()),
            }
        }

        Ok(Self {
            hook,
            ipc_filename,
            watched_paths,
        })
    }
}

fn read_mork_from(r: impl BufRead) -> io::Result<msf::Mork> {
    let mut m = msf::Mork::default();
    msf::Parser::new(r, &mut m).run()?;
    Ok(m)
}

fn read_mork(path: impl AsRef<Path>) -> io::Result<msf::Mork> {
    let f = BufReader::with_capacity(BUF_SIZE, File::open(path)?);
    read_mork_from(f)
}

fn publish(unread: usize, options: &Options) {
    let content = if unread == 0 { format!("\n") } else { format!("{unread} unread\n") };

    if let Err(e) = fs::write(options.ipc_filename.clone(), content) {
        eprintln!("{e}");
        return;
    }

    if let Some(hook) = &options.hook {
        // Maybe polybar isn't running or something, which will cause this command to fail
        let _ = Command::new("sh")
            .args(["-c", hook])
            .stdout(Stdio::null())
            .status();
    }
}

fn unread_messages(paths: Vec<&PathBuf>) -> usize {
    let mut unread = 0;

    for path in paths {
        let m = match read_mork(path) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("{e}");
                continue;
            }
        };
        let report = msf::analyze(&m);
        unread += report.unread.len();
    }

    unread
}

fn main() -> std::io::Result<()> {
    let mut notifier = inotify::Inotify::new()?;
    let Ok(options) = Options::from_args(std::env::args()) else {
        std::process::exit(1);
    };
    if options.watched_paths.len() == 0 {
        eprintln!("{USAGE}");
        std::process::exit(1);
    }
    for path in &options.watched_paths {
        notifier.watch(path)?;
    }

    println!("Watching {} paths", options.watched_paths.len());

    // initial publish
    publish(
        unread_messages(notifier.watched_paths.values().collect()),
        &options,
    );

    loop {
        // spend them all
        for _ in notifier.read_events()? {
        }

        // always check all of them
        publish(
            unread_messages(notifier.watched_paths.values().collect()),
            &options,
        );
    }
}
