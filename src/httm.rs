//       ___           ___           ___           ___
//      /\__\         /\  \         /\  \         /\__\
//     /:/  /         \:\  \        \:\  \       /::|  |
//    /:/__/           \:\  \        \:\  \     /:|:|  |
//   /::\  \ ___       /::\  \       /::\  \   /:/|:|__|__
//  /:/\:\  /\__\     /:/\:\__\     /:/\:\__\ /:/ |::::\__\
//  \/__\:\/:/  /    /:/  \/__/    /:/  \/__/ \/__/~~/:/  /
//       \::/  /    /:/  /        /:/  /            /:/  /
//       /:/  /     \/__/         \/__/            /:/  /
//      /:/  /                                    /:/  /
//      \/__/                                     \/__/
//
// (c) Robert Swinford <robert.swinford<...at...>gmail.com>
//
// For the full copyright and license information, please view the LICENSE file
// that was distributed with this source code.

use clap::{Arg, ArgMatches};
use std::io::BufRead;
use std::time::SystemTime;
use std::{
    error::Error,
    ffi::OsString,
    fmt,
    fs::canonicalize,
    io::Write,
    path::{Path, PathBuf},
};
mod display;
mod interactive;
mod lookup;

use crate::display::{display_pretty, display_raw};
use crate::interactive::interactive_exec;
use crate::lookup::run_search;

#[derive(Debug)]
pub struct HttmError {
    details: String,
}

impl HttmError {
    fn new(msg: &str) -> HttmError {
        HttmError {
            details: msg.to_string(),
        }
    }
}

impl fmt::Display for HttmError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.details)
    }
}

impl Error for HttmError {
    fn description(&self) -> &str {
        &self.details
    }
}

#[derive(Clone)]
pub struct PathData {
    system_time: SystemTime,
    size: u64,
    path_buf: PathBuf,
    is_phantom: bool,
}

impl PathData {
    fn new(path: &Path) -> Option<PathData> {
        let parent = if let Some(parent) = path.parent() {
            parent
        } else {
            Path::new("/")
        };

        let absolute_path: PathBuf = if path.is_relative() {
            if let Ok(pwd) = std::env::var("PWD") {
                [PathBuf::from(&pwd), path.to_path_buf()].iter().collect()
            } else {
                [PathBuf::from("/"), path.to_path_buf()].iter().collect()
            }
        } else if let Ok(can_path) = canonicalize(parent) {
            [can_path, path.to_path_buf()].iter().collect()
        } else {
            [PathBuf::from("/"), path.to_path_buf()].iter().collect()
        };

        let len;
        let time;
        let phantom;

        match std::fs::metadata(&absolute_path) {
            Ok(md) => {
                len = md.len();
                time = md.modified().ok()?;
                phantom = false;
            }
            // this seems like a perfect place for a None value, as the file has no metadata,
            // however we will want certain iters to print the *request*, say for deleted/fake files,
            // so we set up a dummy Some value just so we can have the path names we entered
            //
            // if we get a spurious example of no metadata in snapshot directories we just ignore later
            Err(_) => {
                len = 0u64;
                time = SystemTime::UNIX_EPOCH;
                phantom = true;
            }
        }

        Some(PathData {
            system_time: time,
            size: len,
            path_buf: absolute_path,
            is_phantom: phantom,
        })
    }
}

pub struct Config {
    raw_paths: Vec<String>,
    opt_raw: bool,
    opt_zeros: bool,
    opt_no_pretty: bool,
    opt_no_live_vers: bool,
    opt_interactive: bool,
    opt_restore: bool,
    opt_recursive: bool,
    opt_select: bool,
    opt_snap_point: Option<OsString>,
    opt_local_dir: Option<OsString>,
    current_working_dir: PathBuf,
    user_requested_dir: PathBuf,
}

impl Config {
    fn from(matches: ArgMatches) -> Result<Config, Box<dyn std::error::Error>> {
        let zeros = matches.is_present("ZEROS");
        let raw = matches.is_present("RAW");
        let no_so_pretty = matches.is_present("NOT_SO_PRETTY");
        let no_live_vers = matches.is_present("NO_LIVE");
        let interactive = matches.is_present("INTERACTIVE")
            || matches.is_present("RESTORE")
            || matches.is_present("SELECT");
        let restore = matches.is_present("RESTORE");
        let env_local_dir = std::env::var("HTTM_LOCAL_DIR").ok();
        let recursive = matches.is_present("RECURSIVE");
        let select = matches.is_present("SELECT");

        if recursive && !interactive && !restore {
            return Err(HttmError::new(
                "Recursive search feature only allowed with interactive or restore modes.",
            )
            .into());
        }

        let raw_snap_var = if let Some(raw_value) = matches.value_of_os("SNAP_POINT") {
            Some(raw_value.to_os_string())
        } else if let Ok(env_manual_mnt) = std::env::var("HTTM_SNAP_POINT") {
            Some(OsString::from(env_manual_mnt))
        } else {
            None
        };

        let snap_point = if let Some(raw_value) = raw_snap_var {
            // dir exists sanity check?: check that path contains the hidden snapshot directory
            let snapshot_dir: PathBuf = [&raw_value.to_string_lossy(), ".zfs", "snapshot"]
                .iter()
                .collect();

            if snapshot_dir.metadata().is_ok() {
                Some(raw_value)
            } else {
                return Err(HttmError::new(
                    "Manually set mountpoint does not contain a hidden ZFS directory.  Please mount a ZFS directory there or try another mountpoint.",
                ).into());
            }
        } else {
            None
        };

        let local_dir = if let Some(raw_value) = matches.value_of_os("LOCAL_DIR") {
            // dir exists sanity check?: check path exists by checking for path metadata
            if PathBuf::from(raw_value).metadata().is_ok() {
                Some(raw_value.to_os_string())
            } else {
                return Err(HttmError::new(
                    "Manually set relative directory does not exist.  Please try another.",
                )
                .into());
            }
        } else {
            env_local_dir.map(OsString::from)
        };

        // working dir from env
        let pwd = if let Ok(pwd) = std::env::var("PWD") {
            if let Ok(cp) = PathBuf::from(&pwd).canonicalize() {
                cp
            } else {
                PathBuf::from("/")
            }
        } else {
            PathBuf::from("/")
        };

        let file_names: Vec<String> = if matches.is_present("INPUT_FILES") {
            let raw_values = matches.values_of_os("INPUT_FILES").unwrap();
            raw_values
                .filter_map(|i| i.to_owned().into_string().ok())
                .collect()
        } else if interactive {
            Vec::new()
        } else {
            read_stdin()?
        };

        // is there a user defined working dir given at the cli?
        let requested_dir = if interactive
            && file_names.get(0).is_some()
            && PathBuf::from(file_names.get(0).unwrap()).is_dir()
        {
            PathBuf::from(&file_names.get(0).unwrap())
        } else {
            pwd.clone()
        };

        let config = Config {
            raw_paths: file_names,
            opt_raw: raw,
            opt_zeros: zeros,
            opt_no_pretty: no_so_pretty,
            opt_no_live_vers: no_live_vers,
            opt_snap_point: snap_point,
            opt_local_dir: local_dir,
            opt_recursive: recursive,
            opt_interactive: interactive,
            opt_restore: restore,
            opt_select: select,
            current_working_dir: pwd,
            user_requested_dir: requested_dir,
        };

        Ok(config)
    }
}

fn parse_args() -> ArgMatches {
    clap::Command::new("httm")
        .about("displays information about unique file versions contained on ZFS snapshots, as well as interactive snapshot viewing and restore.\n\n*But don't call it H__ T__ Time Machine.*")
        .version("0.4.0") 
        .arg(
            Arg::new("INPUT_FILES")
                .help("...you should put your files here, if stdin(3) is not your flavor.")
                .takes_value(true)
                .multiple_values(true)
                .display_order(1)
        )
        .arg(
            Arg::new("INTERACTIVE")
                .short('i')
                .long("interactive")
                .help("use native dialogs for an interactive lookup session.")
                .multiple_values(true)
                .display_order(2)
        )
        .arg(
            Arg::new("SELECT")
                .short('s')
                .long("select")
                .help("use native dialogs for an interactive lookup and file select.")
                .multiple_values(true)
                .display_order(3)
        )
        .arg(
            Arg::new("RESTORE")
                .short('r')
                .long("restore")
                .help("use native dialogs for an interactive restore from backup.")
                .multiple_values(true)
                .display_order(4)
        )
        .arg(
            Arg::new("RECURSIVE")
                .short('R')
                .long("recursive")
                .help("recurse into selected directory to find more files. Only available in interactive, select and restore modes.  WARNING: Right now, this method blocks until the recursive search is complete.  So, right now, only useful in directories with fewer (several thousand?) files.")
                .display_order(5)
        )
        .arg(
            Arg::new("SNAP_POINT")
                .long("snap-point")
                .help("ordinarily httm will automatically choose your most local snapshot directory, but here you may manually specify your own mount point for that directory, such as the mount point for a remote share.  You can also set via the environment variable HTTM_SNAP_POINT.")
                .takes_value(true)
                .display_order(6)
        )
        .arg(
            Arg::new("LOCAL_DIR")
                .long("local-dir")
                .help("used with SNAP_POINT to determine where the corresponding live root of the ZFS snapshot dataset is.  If not set, httm defaults to your current working directory.  You can also set via the environment variable HTTM_LOCAL_DIR.")
                .requires("SNAP_POINT")
                .takes_value(true)
                .display_order(7)
        )
        .arg(
            Arg::new("RAW")
                .short('n')
                .long("raw")
                .help("list the backup locations, without extraneous information, delimited by a NEWLINE.")
                .conflicts_with_all(&["ZEROS", "NOT_SO_PRETTY"])
                .display_order(8)
        )
        .arg(
            Arg::new("ZEROS")
                .short('0')
                .long("zero")
                .help("list the backup locations, without extraneous information, delimited by a NULL CHARACTER.")
                .conflicts_with_all(&["RAW", "NOT_SO_PRETTY"])
                .display_order(9)
        )
        .arg(
            Arg::new("NOT_SO_PRETTY")
                .long("not-so-pretty")
                .help("list the backup locations in a parseable format.")
                .conflicts_with_all(&["RAW", "ZEROS"])
                .display_order(10)
        )
        .arg(
            Arg::new("NO_LIVE")
                .long("no-live")
                .help("only display snapshot copies, and no 'live' copies of files or directories.")
                .display_order(11)
        )
        .get_matches()
}

// happy 'lil main fn that only handles errors
fn main() {
    if let Err(e) = exec() {
        eprintln!("error {}", e);
        std::process::exit(1)
    } else {
        std::process::exit(0)
    }
}

fn exec() -> Result<(), Box<dyn std::error::Error>> {
    let mut out = std::io::stdout();
    let arg_matches = parse_args();
    let config = Config::from(arg_matches)?;

    // next, let's do our interactive lookup thing, if appropriate
    // and modify strings returned according to the interactive session
    let raw_paths = interactive_exec(&mut out, &config)?;

    // build pathdata from strings
    let pathdata_set = convert_strings_to_pathdata(&config, &raw_paths)?;

    // finally run search on those paths
    let snaps_and_live_set = run_search(&config, pathdata_set)?;

    // and display
    let output_buf = if config.opt_raw || config.opt_zeros {
        display_raw(&config, snaps_and_live_set)?
    } else {
        display_pretty(&config, snaps_and_live_set)?
    };

    write!(out, "{}", output_buf)?;
    out.flush()?;

    Ok(())
}

fn read_stdin() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut buffer = String::new();
    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();
    stdin.read_line(&mut buffer)?;

    let broken_string: Vec<String> = buffer
        .split_ascii_whitespace()
        .into_iter()
        .map(|i| i.to_owned())
        .collect();

    Ok(broken_string)
}

pub fn convert_strings_to_pathdata(
    config: &Config,
    raw_paths: &[String],
) -> Result<Vec<Option<PathData>>, Box<dyn std::error::Error>> {
    // build our pathdata Vecs for our lookup request
    let vec_pd: Vec<Option<PathData>> = raw_paths
        .iter()
        .map(|string| {
            let path = Path::new(&string);
            if path.is_relative() {
                let wd: PathBuf = [config.user_requested_dir.clone(), path.to_path_buf()]
                    .iter()
                    .collect();
                PathData::new(&wd)
            } else {
                PathData::new(path)
            }
        })
        .collect();

    Ok(vec_pd)
}
