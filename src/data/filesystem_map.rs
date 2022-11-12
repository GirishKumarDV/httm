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

use crate::data::paths::PathData;
use crate::lookup::versions::MostProximateAndOptAlts;
use std::{collections::BTreeMap, path::PathBuf};

pub type MapOfDatasets = BTreeMap<PathBuf, DatasetMetadata>;
pub type MapOfSnaps = BTreeMap<PathBuf, Vec<PathBuf>>;
pub type MapOfAlts = BTreeMap<PathBuf, MostProximateAndOptAlts>;
pub type MapOfAliases = BTreeMap<PathBuf, RemotePathAndFsType>;
pub type MapLiveToSnaps = BTreeMap<PathData, Vec<PathData>>;
pub type DisplaySet = [Vec<PathData>; 2];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FilesystemType {
    Zfs,
    Btrfs,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MountType {
    Local,
    Network,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePathAndFsType {
    pub remote_dir: PathBuf,
    pub fs_type: FilesystemType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetMetadata {
    pub name: String,
    pub fs_type: FilesystemType,
    pub mount_type: MountType,
}

#[derive(Copy, Debug, Clone, PartialEq, Eq)]
pub enum SnapDatasetType {
    MostProximate,
    AltReplicated,
}

#[derive(Copy, Debug, Clone, PartialEq, Eq)]
pub enum SnapsSelectedForSearch {
    MostProximateOnly,
    IncludeAltReplicated,
}

// alt replicated should come first,
// so as to be at the top of results
static INCLUDE_ALTS: &[SnapDatasetType] = [
    SnapDatasetType::AltReplicated,
    SnapDatasetType::MostProximate,
]
.as_slice();

static ONLY_PROXIMATE: &[SnapDatasetType] = [SnapDatasetType::MostProximate].as_slice();

impl SnapsSelectedForSearch {
    pub fn get_value(&self) -> &[SnapDatasetType] {
        match self {
            SnapsSelectedForSearch::IncludeAltReplicated => INCLUDE_ALTS,
            SnapsSelectedForSearch::MostProximateOnly => ONLY_PROXIMATE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetCollection {
    // key: mount, val: (dataset/subvol, fs_type, mount_type)
    pub map_of_datasets: MapOfDatasets,
    // key: mount, val: vec snap locations on disk (e.g. /.zfs/snapshot/snap_8a86e4fc_prepApt/home)
    pub map_of_snaps: MapOfSnaps,
    // key: mount, val: alt dataset
    pub opt_map_of_alts: Option<MapOfAlts>,
    // key: local dir, val: (remote dir, fstype)
    pub opt_map_of_aliases: Option<MapOfAliases>,
    // vec dirs to be filtered
    pub vec_of_filter_dirs: Vec<PathBuf>,
    // opt single dir to to be filtered re: btrfs common snap dir
    pub opt_common_snap_dir: Option<PathBuf>,
    // vec of two enum variants - most proximate and alt replicated, or just most proximate
    pub snaps_selected_for_search: SnapsSelectedForSearch,
}
