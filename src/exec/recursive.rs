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
// Copyright (c) 2023, Robert Swinford <robert.swinford<...at...>gmail.com>
//
// For the full copyright and license information, please view the LICENSE file
// that was distributed with this source code.

use std::ops::Deref;
use std::{fs::read_dir, path::Path, sync::Arc};

use rayon::{Scope, ThreadPool};
use skim::prelude::*;

use crate::config::generate::{DeletedMode, ExecMode};
use crate::data::paths::{BasicDirEntryInfo, PathData};
use crate::data::selection::SelectionCandidate;
use crate::display_versions::wrapper::VersionsDisplayWrapper;
use crate::exec::deleted::SpawnDeletedThread;
use crate::library::results::{HttmError, HttmResult};
use crate::library::utility::is_channel_closed;
use crate::library::utility::{print_output_buf, HttmIsDir, Never};
use crate::parse::mounts::MaxLen;
use crate::VersionsMap;
use crate::GLOBAL_CONFIG;
use crate::{BTRFS_SNAPPER_HIDDEN_DIRECTORY, ZFS_HIDDEN_DIRECTORY};

pub struct RecursiveSearch;

impl RecursiveSearch {
    pub fn exec(requested_dir: &Path, skim_tx: SkimItemSender, hangup_rx: Receiver<Never>) {
        if GLOBAL_CONFIG.opt_deleted_mode.is_some() {
            // thread pool allows deleted to have its own scope, which means
            // all threads must complete before the scope exits.  this is important
            // for display recursive searches as the live enumeration will end before
            // all deleted threads have completed
            let pool: ThreadPool = rayon::ThreadPoolBuilder::new()
                .build()
                .expect("Could not initialize rayon threadpool for recursive deleted search");

            pool.in_place_scope(|deleted_scope| {
                Self::run_enumerate_loop(requested_dir, skim_tx, hangup_rx, Some(deleted_scope))
            })
        } else {
            Self::run_enumerate_loop(requested_dir, skim_tx, hangup_rx, None)
        }
    }

    fn run_enumerate_loop(
        requested_dir: &Path,
        skim_tx: SkimItemSender,
        hangup_rx: Receiver<Never>,
        opt_deleted_scope: Option<&Scope>,
    ) {
        // this runs the main loop for live file searches, see the referenced struct below
        // we are in our own detached system thread, so print error and exit if error trickles up
        RecursiveMainLoop::exec(requested_dir, opt_deleted_scope, &skim_tx, &hangup_rx)
            .unwrap_or_else(|error| {
                eprintln!("Error: {error}");
                std::process::exit(1)
            });
    }
}

// this is the main loop to recurse all files
pub struct RecursiveMainLoop;

impl RecursiveMainLoop {
    fn exec(
        requested_dir: &Path,
        opt_deleted_scope: Option<&Scope>,
        skim_tx: &SkimItemSender,
        hangup_rx: &Receiver<Never>,
    ) -> HttmResult<()> {
        // runs once for non-recursive but also "primes the pump"
        // for recursive to have items available, also only place an
        // error can stop execution
        let mut queue: Vec<BasicDirEntryInfo> =
            Self::new(requested_dir, opt_deleted_scope, skim_tx, hangup_rx)?;

        if GLOBAL_CONFIG.opt_recursive {
            // condition kills iter when user has made a selection
            // pop_back makes this a LIFO queue which is supposedly better for caches
            while let Some(item) = queue.pop() {
                // check -- should deleted threads keep working?
                // exit/error on disconnected channel, which closes
                // at end of browse scope
                if is_channel_closed(hangup_rx) {
                    break;
                }

                // no errors will be propagated in recursive mode
                // far too likely to run into a dir we don't have permissions to view
                if let Ok(mut item) = Self::new(&item.path, opt_deleted_scope, skim_tx, hangup_rx) {
                    queue.append(&mut item)
                }
            }
        }

        Ok(())
    }

    #[allow(clippy::new_ret_no_self)]
    fn new(
        requested_dir: &Path,
        opt_deleted_scope: Option<&Scope>,
        skim_tx: &SkimItemSender,
        hangup_rx: &Receiver<Never>,
    ) -> HttmResult<Vec<BasicDirEntryInfo>> {
        // combined entries will be sent or printed, but we need the vec_dirs to recurse
        let (vec_dirs, vec_files): (Vec<BasicDirEntryInfo>, Vec<BasicDirEntryInfo>) =
            SharedRecursive::get_entries_partitioned(requested_dir)?;

        SharedRecursive::combine_and_send_entries(
            vec_files,
            &vec_dirs,
            false,
            requested_dir,
            skim_tx,
        )?;

        if let Some(deleted_scope) = opt_deleted_scope {
            SpawnDeletedThread::exec(requested_dir, deleted_scope, skim_tx, hangup_rx);
        }

        Ok(vec_dirs)
    }
}

pub struct SharedRecursive;

impl SharedRecursive {
    pub fn combine_and_send_entries(
        vec_files: Vec<BasicDirEntryInfo>,
        vec_dirs: &[BasicDirEntryInfo],
        is_phantom: bool,
        requested_dir: &Path,
        skim_tx: &SkimItemSender,
    ) -> HttmResult<()> {
        let mut combined = vec_files;
        combined.extend_from_slice(vec_dirs);

        let entries = if is_phantom {
            // deleted - phantom
            Self::get_pseudo_live_versions(combined, requested_dir)
        } else {
            // live - not phantom
            match GLOBAL_CONFIG.opt_deleted_mode {
                Some(DeletedMode::Only) => return Ok(()),
                Some(DeletedMode::DepthOfOne | DeletedMode::All) | None => {
                    // never show live files is display recursive/deleted only file mode
                    if matches!(
                        GLOBAL_CONFIG.exec_mode,
                        ExecMode::NonInteractiveRecursive(_)
                    ) {
                        return Ok(());
                    }
                    combined
                }
            }
        };

        Self::display_or_transmit(entries, is_phantom, skim_tx)
    }

    pub fn get_entries_partitioned(
        requested_dir: &Path,
    ) -> HttmResult<(Vec<BasicDirEntryInfo>, Vec<BasicDirEntryInfo>)> {
        // separates entries into dirs and files
        let (vec_dirs, vec_files) = read_dir(requested_dir)?
            .flatten()
            // checking file_type on dir entries is always preferable
            // as it is much faster than a metadata call on the path
            .map(|dir_entry| BasicDirEntryInfo::from(&dir_entry))
            .filter(|entry| {
                if GLOBAL_CONFIG.opt_no_filter {
                    return true;
                } else if GLOBAL_CONFIG.opt_no_hidden
                    && entry.get_filename().to_string_lossy().starts_with('.')
                {
                    return false;
                } else if let Ok(file_type) = entry.get_filetype() {
                    if file_type.is_dir() {
                        return !Self::is_filter_dir(entry);
                    }
                }
                true
            })
            .partition(Self::is_entry_dir);

        Ok((vec_dirs, vec_files))
    }

    pub fn is_entry_dir(entry: &BasicDirEntryInfo) -> bool {
        // must do is_dir() look up on DirEntry file_type() as look up on Path will traverse links!
        if GLOBAL_CONFIG.opt_no_traverse {
            if let Ok(file_type) = entry.get_filetype() {
                return file_type.is_dir();
            }
        }

        entry.httm_is_dir()
    }

    fn is_filter_dir(entry: &BasicDirEntryInfo) -> bool {
        // FYI path is always a relative path, but no need to canonicalize as
        // partial eq for paths is comparison of components iter
        let path = entry.path.as_path();

        // never check the hidden snapshot directory for live files (duh)
        // didn't think this was possible until I saw a SMB share return
        // a .zfs dir entry
        if path.ends_with(ZFS_HIDDEN_DIRECTORY) || path.ends_with(BTRFS_SNAPPER_HIDDEN_DIRECTORY) {
            return true;
        }

        // is a common btrfs snapshot dir?
        if let Some(common_snap_dir) = &GLOBAL_CONFIG.dataset_collection.opt_common_snap_dir {
            if path == *common_snap_dir {
                return true;
            }
        }

        // check whether user requested this dir specifically, then we will show
        if let Some(user_requested_dir) = GLOBAL_CONFIG.opt_requested_dir.as_ref() {
            if user_requested_dir.path_buf.as_path() == path {
                return false;
            }
        }

        // finally : is a non-supported dataset?
        // bailout easily if path is larger than max_filter_dir len
        if path.components().count() > GLOBAL_CONFIG.dataset_collection.filter_dirs.get_max_len() {
            return false;
        }

        GLOBAL_CONFIG
            .dataset_collection
            .filter_dirs
            .deref()
            .contains(path)
    }

    // this function creates dummy "live versions" values to match deleted files
    // which have been found on snapshots, we return to the user "the path that
    // once was" in their browse panel
    fn get_pseudo_live_versions(
        entries: Vec<BasicDirEntryInfo>,
        pseudo_live_dir: &Path,
    ) -> Vec<BasicDirEntryInfo> {
        entries
            .into_iter()
            .map(|basic_info| BasicDirEntryInfo {
                path: pseudo_live_dir.join(basic_info.path.file_name().unwrap_or_default()),
                file_type: basic_info.file_type,
            })
            .collect()
    }

    fn display_or_transmit(
        entries: Vec<BasicDirEntryInfo>,
        is_phantom: bool,
        skim_tx: &SkimItemSender,
    ) -> HttmResult<()> {
        // send to the interactive view, or print directly, never return back
        match &GLOBAL_CONFIG.exec_mode {
            ExecMode::Interactive(_) => Self::transmit(entries, is_phantom, skim_tx)?,
            ExecMode::NonInteractiveRecursive(progress_bar) => {
                if entries.is_empty() {
                    if GLOBAL_CONFIG.opt_recursive {
                        progress_bar.tick();
                    } else {
                        eprintln!(
                            "NOTICE: httm could not find any deleted files at this directory level.  \
                        Perhaps try specifying a deleted mode in combination with \"--recursive\"."
                        )
                    }
                } else {
                    NonInteractiveRecursiveWrapper::print(entries)?;

                    // keeps spinner from squashing last line of output
                    if GLOBAL_CONFIG.opt_recursive {
                        eprintln!();
                    }
                }
            }
            _ => unreachable!(),
        }

        Ok(())
    }

    fn transmit(
        entries: Vec<BasicDirEntryInfo>,
        is_phantom: bool,
        skim_tx: &SkimItemSender,
    ) -> HttmResult<()> {
        // don't want a par_iter here because it will block and wait for all
        // results, instead of printing and recursing into the subsequent dirs
        entries
            .into_iter()
            .try_for_each(|basic_info| {
                skim_tx.try_send(Arc::new(SelectionCandidate::new(basic_info, is_phantom)))
            })
            .map_err(std::convert::Into::into)
    }
}

// this is wrapper for non-interactive searches, which will be executed through the SharedRecursive fns
// here we disable the skim transmitter, etc., because we will simply be printing anything we find
pub struct NonInteractiveRecursiveWrapper;

impl NonInteractiveRecursiveWrapper {
    #[allow(unused_variables)]
    pub fn exec() -> HttmResult<()> {
        // won't be sending anything anywhere, this just allows us to reuse enumerate_directory
        let (dummy_skim_tx, _): (SkimItemSender, SkimItemReceiver) = unbounded();
        let (hangup_tx, hangup_rx): (Sender<Never>, Receiver<Never>) = bounded(0);

        match &GLOBAL_CONFIG.opt_requested_dir {
            Some(requested_dir) => {
                RecursiveSearch::exec(&requested_dir.path_buf, dummy_skim_tx, hangup_rx);
            }
            None => {
                return Err(HttmError::new(
                    "requested_dir should never be None in Display Recursive mode",
                )
                .into())
            }
        }

        Ok(())
    }

    fn print(entries: Vec<BasicDirEntryInfo>) -> HttmResult<()> {
        let pseudo_live_set: Vec<PathData> = entries.iter().map(PathData::from).collect();

        let versions_map = VersionsMap::new(&GLOBAL_CONFIG, &pseudo_live_set)?;
        let output_buf = VersionsDisplayWrapper::from(&GLOBAL_CONFIG, versions_map).to_string();

        print_output_buf(output_buf)
    }
}
