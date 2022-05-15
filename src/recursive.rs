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

use std::{ffi::OsStr, fs::read_dir, io::Write, path::Path, sync::Arc};

use rayon::{iter::Either, prelude::*};
use skim::prelude::*;

use crate::deleted::get_unique_deleted;
use crate::display::display_exec;
use crate::interactive::SelectionCandidate;
use crate::lookup::get_versions;
use crate::utility::httm_is_dir;
use crate::{
    BasicDirEntryInfo, Config, DeletedMode, ExecMode, HttmError, PathData, ZFS_HIDDEN_DIRECTORY,
};

pub fn display_recursive_exec(
    config: &Config,
) -> Result<[Vec<PathData>; 2], Box<dyn std::error::Error + Send + Sync + 'static>> {
    // won't be sending anything anywhere, this just allows us to reuse enumerate_directory
    let (dummy_tx_item, _): (SkimItemSender, SkimItemReceiver) = unbounded();
    let config_clone = Arc::new(config.clone());

    enumerate_directory(config_clone, &dummy_tx_item, &config.requested_dir.path_buf)?;

    // flush and exit successfully upon ending recursive search
    if config.opt_recursive {
        let mut out = std::io::stdout();
        writeln!(out)?;
        out.flush()?;
    }
    std::process::exit(0)
}

pub fn enumerate_directory(
    config: Arc<Config>,
    tx_item: &SkimItemSender,
    requested_dir: &Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    let (vec_dirs, vec_files): (Vec<BasicDirEntryInfo>, Vec<BasicDirEntryInfo>) =
        read_dir(&requested_dir)?
            .flatten()
            .par_bridge()
            // never check the hidden snapshot directory for live files (duh)
            // didn't think this was possible until I saw a SMB share return
            // a .zfs dir entry
            .filter(|dir_entry| {
                dir_entry.file_name().as_os_str() != OsStr::new(ZFS_HIDDEN_DIRECTORY)
            })
            // checking file_type on dir entries is always preferable
            // as it is much faster than a metadata call on the path
            .partition_map(|dir_entry| {
                let res = BasicDirEntryInfo {
                    file_name: dir_entry.file_name(),
                    path: dir_entry.path(),
                    file_type: dir_entry.file_type().ok(),
                };
                if httm_is_dir(&dir_entry) {
                    Either::Left(res)
                } else {
                    Either::Right(res)
                }
            });

    let spawn_enumerate_deleted = || {
        let config_clone = config.clone();
        let requested_dir_clone = requested_dir.to_path_buf();
        let tx_item_clone = tx_item.clone();

        let _ = enumerate_deleted(config_clone, &requested_dir_clone, &tx_item_clone);
    };

    match config.exec_mode {
        ExecMode::Display => unreachable!(),
        ExecMode::DisplayRecursive => {
            match config.deleted_mode {
                // display recursive in DeletedMode::Disabled may be
                // something to implement in the future but I'm not sure
                // it really makes sense, as it's only really good for a
                // small subset of files
                DeletedMode::Disabled => unreachable!(),
                // for all other non-disabled DeletedModes
                DeletedMode::DepthOfOne | DeletedMode::Enabled | DeletedMode::Only => {
                    // flush and exit successfully upon ending recursive search
                    spawn_enumerate_deleted();
                }
            }
        }
        ExecMode::Interactive => {
            // combine dirs and files into a vec and sort to display
            let combined_vec: Vec<&BasicDirEntryInfo> = match config.deleted_mode {
                DeletedMode::Only => {
                    spawn_enumerate_deleted();
                    // spawn_enumerate_deleted will send deleted files back to
                    // the main thread for us, so we can skip collecting deleted here
                    // and return an empty vec
                    Vec::new()
                }
                DeletedMode::DepthOfOne | DeletedMode::Enabled => {
                    spawn_enumerate_deleted();
                    // spawn_enumerate_deleted will send deleted files back to
                    // the main thread for us, so we can skip collecting a
                    // vec_deleted here
                    [&vec_files, &vec_dirs].into_par_iter().flatten().collect()
                }
                DeletedMode::Disabled => {
                    [&vec_files, &vec_dirs].into_par_iter().flatten().collect()
                }
            };

            // don't want a par_iter here because it will block and wait for all
            // results, instead of printing and recursing into the subsequent dirs
            combined_vec.iter().for_each(|basic_dir_entry_info| {
                let _ = tx_item.send(Arc::new(SelectionCandidate::new(
                    config.clone(),
                    basic_dir_entry_info.path.to_owned(),
                    basic_dir_entry_info.file_type,
                    // know this is non-phantom because obtained through dir entry
                    false,
                )));
            });
        }
    }

    // now recurse into those dirs, if requested
    if config.opt_recursive {
        // don't want a par_iter here because it will block and wait for all
        // results, instead of printing and recursing into the subsequent dirs
        vec_dirs.iter().for_each(|requested_dir| {
            let _ = enumerate_directory(config.clone(), tx_item, &requested_dir.path);
        });
    }

    Ok(())
}

// why another fn? so we can spawn another thread and not block on finding deleted files
// which generally takes a long time.
fn enumerate_deleted(
    config: Arc<Config>,
    requested_dir: &Path,
    tx_item: &SkimItemSender,
) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    let deleted = get_unique_deleted(&config, requested_dir)?;

    let (vec_dirs, vec_files): (Vec<BasicDirEntryInfo>, Vec<BasicDirEntryInfo>) = deleted
        .into_par_iter()
        .partition(|basic_dir_entry_info| httm_is_dir(basic_dir_entry_info));

    // need something to hold our thread handles that we need to wait to complete,
    // also very helpful in the case we don't don't needs threads as it can be empty
    let mut vec_handles = Vec::new();

    if config.deleted_mode != DeletedMode::DepthOfOne {
        let _ = &vec_dirs
            .iter()
            .map(|basic_dir_entry_info| basic_dir_entry_info.path.to_owned())
            .for_each(|deleted_dir| {
                let config_clone = config.clone();
                let requested_dir_clone = requested_dir.to_path_buf();
                let tx_item_clone = tx_item.clone();

                // thread spawn fn enumerate_directory - permits recursion into dirs without blocking
                // or blocking when we choose to block
                let handle = std::thread::spawn(move || {
                    let _ = behind_deleted_dir(
                        config_clone,
                        &tx_item_clone,
                        &deleted_dir,
                        &requested_dir_clone,
                    );
                });

                vec_handles.push(handle);
            });
    }

    // these are dummy "live versions" values generated to match deleted files
    // which have been found on snapshots, we return to the user "the path that
    // once was" in their browse panel
    let pseudo_live_versions: Vec<BasicDirEntryInfo> = [&vec_dirs, &vec_files]
        .into_par_iter()
        .flatten()
        .cloned()
        .collect();

    match config.exec_mode {
        ExecMode::Interactive => send_deleted_recursive(config, &pseudo_live_versions, tx_item)?,
        ExecMode::DisplayRecursive => {
            // here we make sure to wait until all child threads have exited before returning
            // this allows the main work of the fn to keep running while while work on deleted
            // files in the background
            let _ = vec_handles.into_iter().try_for_each(|handle| handle.join());

            if !pseudo_live_versions.is_empty() {
                if config.opt_recursive {
                    eprintln!();
                }
                print_deleted_recursive(config, &pseudo_live_versions)?
            } else if config.opt_recursive {
                eprint!(".");
            }
        }
        _ => unreachable!(),
    }

    Ok(())
}

fn behind_deleted_dir(
    config: Arc<Config>,
    tx_item: &SkimItemSender,
    deleted_dir: &Path,
    requested_dir: &Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    fn recurse_behind_deleted_dir(
        config: Arc<Config>,
        tx_item: &SkimItemSender,
        dir_name: &Path,
        from_deleted_dir: &Path,
        from_requested_dir: &Path,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
        let deleted_dir_on_snap = &from_deleted_dir.to_path_buf().join(&dir_name);
        let pseudo_live_dir = &from_requested_dir.to_path_buf().join(&dir_name);

        let (vec_dirs, vec_files): (Vec<BasicDirEntryInfo>, Vec<BasicDirEntryInfo>) =
            read_dir(&deleted_dir_on_snap)?
                .flatten()
                .par_bridge()
                .partition_map(|dir_entry| {
                    let res = BasicDirEntryInfo {
                        file_name: dir_entry.file_name(),
                        path: dir_entry.path(),
                        file_type: dir_entry.file_type().ok(),
                    };
                    if httm_is_dir(&dir_entry) {
                        Either::Left(res)
                    } else {
                        Either::Right(res)
                    }
                });

        let pseudo_live_versions: Vec<BasicDirEntryInfo> = [&vec_files, &vec_dirs]
            .into_par_iter()
            .flatten()
            .filter_map(|basic_dir_entry_info| {
                basic_dir_entry_info
                    .path
                    .file_name()
                    .map(|file_name| BasicDirEntryInfo {
                        file_name: file_name.to_os_string(),
                        path: pseudo_live_dir.join(file_name),
                        file_type: basic_dir_entry_info.file_type,
                    })
            })
            .collect();

        match config.exec_mode {
            ExecMode::Interactive => {
                send_deleted_recursive(config.clone(), &pseudo_live_versions, tx_item)?
            }
            ExecMode::DisplayRecursive => {
                if !pseudo_live_versions.is_empty() {
                    if config.opt_recursive {
                        eprintln!();
                    }
                    print_deleted_recursive(config.clone(), &pseudo_live_versions)?
                } else if config.opt_recursive {
                    eprint!(".");
                }
            }
            _ => unreachable!(),
        }

        vec_dirs.into_iter().for_each(|basic_dir_entry_info| {
            let _ = recurse_behind_deleted_dir(
                config.clone(),
                tx_item,
                Path::new(&basic_dir_entry_info.file_name),
                deleted_dir_on_snap,
                pseudo_live_dir,
            );
        });

        Ok(())
    }

    match &deleted_dir.file_name() {
        Some(dir_name) => recurse_behind_deleted_dir(
            config,
            tx_item,
            Path::new(dir_name),
            deleted_dir.parent().unwrap_or_else(|| Path::new("/")),
            requested_dir,
        )?,
        None => return Err(HttmError::new("Not a valid file!").into()),
    }

    Ok(())
}

fn send_deleted_recursive(
    config: Arc<Config>,
    pathdata: &[BasicDirEntryInfo],
    tx_item: &SkimItemSender,
) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    pathdata.iter().for_each(|basic_dir_entry_info| {
        let _ = tx_item.send(Arc::new(SelectionCandidate::new(
            config.clone(),
            basic_dir_entry_info.path.to_owned(),
            basic_dir_entry_info.file_type,
            // know this is_phantom because we know it is deleted
            true,
        )));
    });
    Ok(())
}

fn print_deleted_recursive(
    config: Arc<Config>,
    path_buf_set: &[BasicDirEntryInfo],
) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    let pseudo_live_set: Vec<PathData> = path_buf_set
        .iter()
        .map(|basic_dir_entry_info| PathData::from(basic_dir_entry_info.path.as_path()))
        .collect();

    let snaps_and_live_set = get_versions(&config, &pseudo_live_set)?;

    let mut out = std::io::stdout();
    let output_buf = display_exec(&config, snaps_and_live_set)?;
    let _ = write!(out, "{}", output_buf);
    out.flush()?;

    Ok(())
}
