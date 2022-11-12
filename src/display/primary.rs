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

use std::borrow::Cow;
use std::ops::Deref;

use number_prefix::NumberPrefix;
use terminal_size::{terminal_size, Height, Width};

use crate::config::generate::{Config, ExecMode};
use crate::data::paths::{PathData, PHANTOM_DATE, PHANTOM_SIZE};
use crate::library::results::HttmResult;
use crate::library::utility::print_output_buf;
use crate::library::utility::{get_date, get_delimiter, paint_string, DateFormat};
use crate::lookup::versions::MapLiveToSnaps;

// 2 space wide padding - used between date and size, and size and path
pub const PRETTY_FIXED_WIDTH_PADDING: &str = "  ";
// our FIXED_WIDTH_PADDING is used twice
pub const PRETTY_FIXED_WIDTH_PADDING_LEN_X2: usize = PRETTY_FIXED_WIDTH_PADDING.len() * 2;
// tab padding used in not so pretty
pub const NOT_SO_PRETTY_FIXED_WIDTH_PADDING: &str = "\t";
// and we add 2 quotation marks to the path when we format
pub const QUOTATION_MARKS_LEN: usize = 2;

impl MapLiveToSnaps {
    pub fn display(&self, config: &Config) -> HttmResult<String> {
        let output_buffer = match &config.exec_mode {
            ExecMode::NumVersions(num_versions_mode) => {
                self.print_num_versions(config, num_versions_mode)
            }
            _ => {
                if config.opt_raw || config.opt_zeros {
                    self.print_raw(config)
                } else {
                    self.print_formatted(config)
                }
            }
        };

        Ok(output_buffer)
    }

    pub fn display_map(&self, config: &Config) -> HttmResult<()> {
        let output_buf = if config.opt_raw || config.opt_zeros {
            self.print_raw(config)
        } else {
            self.print_formatted_map(config)
        };

        print_output_buf(output_buf)
    }

    fn print_raw(&self, config: &Config) -> String {
        let delimiter = get_delimiter(config);

        let write_out_buffer = DisplaySet::new(config, self)
            .iter()
            .flatten()
            .map(|pathdata| format!("{}{}", pathdata.path_buf.display(), delimiter))
            .collect::<String>();

        write_out_buffer
    }

    fn print_formatted(&self, config: &Config) -> String {
        let global_display_set = DisplaySet::new(config, self);
        let global_padding_collection = PaddingCollection::new(config, &global_display_set);

        if self.len() == 1 {
            global_display_set.display(config, &global_padding_collection)
        } else {
            self.deref()
                .clone()
                .into_iter()
                .map(|raw_tuple| raw_tuple.into())
                .map(|raw_instance_set| DisplaySet::new(config, &raw_instance_set))
                .map(|display_set| display_set.display(config, &global_padding_collection))
                .collect::<String>()
        }
    }
    
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DisplaySet {
    inner: [Vec<PathData>; 2],
}

impl From<[Vec<PathData>; 2]> for DisplaySet {
    fn from(array: [Vec<PathData>; 2]) -> Self {
        Self { inner: array }
    }
}

impl From<DisplaySet> for [Vec<PathData>; 2] {
    fn from(display_set: DisplaySet) -> Self {
        display_set.inner
    }
}

impl Deref for DisplaySet {
    type Target = [Vec<PathData>; 2];

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DisplaySet {
    pub fn new(config: &Config, map_live_to_snaps: &MapLiveToSnaps) -> DisplaySet {
        let vec_snaps = if config.opt_no_snap {
            Vec::new()
        } else {
            map_live_to_snaps.values().flatten().cloned().collect()
        };

        let vec_live = if config.opt_last_snap.is_some()
            || config.opt_no_live
            || matches!(config.exec_mode, ExecMode::MountsForFiles)
        {
            Vec::new()
        } else {
            map_live_to_snaps.keys().cloned().collect()
        };

        Self {
            inner: [vec_snaps, vec_live],
        }
    }

    fn display(self, config: &Config, global_padding_collection: &PaddingCollection) -> String {
        // get the display buffer for each set snaps and live
        self.iter().enumerate().fold(
            String::new(),
            |mut display_set_buffer, (idx, snap_or_live_set)| {
                // a DisplaySet is an array of 2 - idx 0 are the snaps, 1 is the live versions
                let is_snap_set = idx == 0;
                let is_live_set = idx == 1;

                let component_buffer: String = snap_or_live_set
                    .iter()
                    .map(|pathdata| {
                        pathdata.display(config, is_live_set, global_padding_collection)
                    })
                    .collect();

                // add each buffer to the set - print fancy border string above, below and between sets
                if config.opt_no_pretty {
                    display_set_buffer += &component_buffer;
                } else if is_snap_set {
                    display_set_buffer += &global_padding_collection.fancy_border_string;
                    if !component_buffer.is_empty() {
                        display_set_buffer += &component_buffer;
                        display_set_buffer += &global_padding_collection.fancy_border_string;
                    }
                } else {
                    display_set_buffer += &component_buffer;
                    display_set_buffer += &global_padding_collection.fancy_border_string;
                }
                display_set_buffer
            },
        )
    }
}

impl PathData {
    fn display(
        &self,
        config: &Config,
        is_live_set: bool,
        padding_collection: &PaddingCollection,
    ) -> String {
        // obtain metadata for timestamp and size
        let metadata = self.md_infallible();

        // tab delimited if "no pretty", no border lines, and no colors
        let (display_size, display_path, display_padding) = if config.opt_no_pretty {
            // displays blanks for phantom values, equaling their dummy lens and dates.
            //
            // we use a dummy instead of a None value here.  Basically, sometimes, we want
            // to print the request even if a live file does not exist
            let size = if self.metadata.is_some() {
                display_human_size(&metadata.size)
            } else {
                padding_collection.phantom_size_pad_str.clone()
            };
            let path = self.path_buf.to_string_lossy();
            let padding = NOT_SO_PRETTY_FIXED_WIDTH_PADDING;
            (size, path, padding)
        // print with padding and pretty border lines and ls colors
        } else {
            let size = {
                let size = if self.metadata.is_some() {
                    display_human_size(&metadata.size)
                } else {
                    padding_collection.phantom_size_pad_str.clone()
                };
                format!(
                    "{:>width$}",
                    size,
                    width = padding_collection.size_padding_len
                )
            };
            let path = {
                let path_buf = &self.path_buf;
                // paint the live strings with ls colors - idx == 1 is 2nd or live set
                let painted_path_str = if is_live_set {
                    paint_string(self, path_buf.to_str().unwrap_or_default())
                } else {
                    path_buf.to_string_lossy()
                };
                Cow::Owned(format!(
                    "\"{:<width$}\"",
                    painted_path_str,
                    width = padding_collection.size_padding_len
                ))
            };
            // displays blanks for phantom values, equaling their dummy lens and dates.
            let padding = PRETTY_FIXED_WIDTH_PADDING;
            (size, path, padding)
        };

        let display_date = if self.metadata.is_some() {
            get_date(config, &metadata.modify_time, DateFormat::Display)
        } else {
            padding_collection.phantom_date_pad_str.to_owned()
        };

        format!(
            "{}{}{}{}{}\n",
            display_date, display_padding, display_size, display_padding, display_path
        )
    }
}

struct PaddingCollection {
    size_padding_len: usize,
    fancy_border_string: String,
    phantom_date_pad_str: String,
    phantom_size_pad_str: String,
}

impl PaddingCollection {
    fn new(config: &Config, display_set: &DisplaySet) -> PaddingCollection {
        // calculate padding and borders for display later
        let (size_padding_len, fancy_border_len) = display_set.iter().flatten().fold(
            (0usize, 0usize),
            |(mut size_padding_len, mut fancy_border_len), pathdata| {
                let metadata = pathdata.md_infallible();

                let (display_date, display_size, display_path) = {
                    let date = get_date(config, &metadata.modify_time, DateFormat::Display);
                    let size = format!(
                        "{:>width$}",
                        display_human_size(&metadata.size),
                        width = size_padding_len
                    );
                    let path = pathdata.path_buf.to_string_lossy();

                    (date, size, path)
                };

                let display_size_len = display_human_size(&metadata.size).len();
                let formatted_line_len = display_date.len()
                    + display_size.len()
                    + display_path.len()
                    + PRETTY_FIXED_WIDTH_PADDING_LEN_X2
                    + QUOTATION_MARKS_LEN;

                size_padding_len = display_size_len.max(size_padding_len);
                fancy_border_len = formatted_line_len.max(fancy_border_len);
                (size_padding_len, fancy_border_len)
            },
        );

        let fancy_border_string: String = Self::get_fancy_border_string(fancy_border_len);

        let phantom_date_pad_str = format!(
            "{:<width$}",
            "",
            width = get_date(config, &PHANTOM_DATE, DateFormat::Display).len()
        );
        let phantom_size_pad_str = format!(
            "{:<width$}",
            "",
            width = display_human_size(&PHANTOM_SIZE).len()
        );

        PaddingCollection {
            size_padding_len,
            fancy_border_string,
            phantom_date_pad_str,
            phantom_size_pad_str,
        }
    }

    fn get_fancy_border_string(fancy_border_len: usize) -> String {
        let get_max_sized_border = || {
            // Active below is the most idiomatic Rust, but it maybe slower than the commented portion
            // (0..fancy_border_len).map(|_| "─").collect()
            format!("{:─<width$}\n", "", width = fancy_border_len)
        };

        match terminal_size() {
            Some((Width(width), Height(_height))) => {
                if (width as usize) < fancy_border_len {
                    // Active below is the most idiomatic Rust, but it maybe slower than the commented portion
                    // (0..width as usize).map(|_| "─").collect()
                    format!("{:─<width$}\n", "", width = width as usize)
                } else {
                    get_max_sized_border()
                }
            }
            None => get_max_sized_border(),
        }
    }
}

fn display_human_size(size: &u64) -> String {
    let size = *size as f64;

    match NumberPrefix::binary(size) {
        NumberPrefix::Standalone(bytes) => {
            format!("{} bytes", bytes)
        }
        NumberPrefix::Prefixed(prefix, n) => {
            format!("{:.1} {}B", n, prefix)
        }
    }
}
