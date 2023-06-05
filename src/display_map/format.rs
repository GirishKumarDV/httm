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

use crate::config::generate::{ExecMode, PrintMode};
use crate::display_map::helper::PrintAsMap;
use crate::display_versions::format::NOT_SO_PRETTY_FIXED_WIDTH_PADDING;
use crate::library::utility::delimiter;
use crate::GLOBAL_CONFIG;

impl std::string::ToString for PrintAsMap {
    fn to_string(&self) -> String {
        if GLOBAL_CONFIG.opt_json {
            let json_string = self.to_json();

            let res = match &GLOBAL_CONFIG.exec_mode {
                ExecMode::Display | ExecMode::Interactive(_) => {
                    json_string.replace("\"inner\": ", "\"versions\": ")
                }
                ExecMode::MountsForFiles(_) => json_string.replace("\"inner\": ", "\"mounts\": "),
                ExecMode::SnapsForFiles(_) => {
                    json_string.replace("\"inner\": ", "\"snapshot_names\": ")
                }
                ExecMode::NonInteractiveRecursive(_)
                | ExecMode::RollForward(_)
                | ExecMode::NumVersions(_)
                | ExecMode::Purge(_)
                | ExecMode::SnapFileMount(_) => {
                    unreachable!(
                        "JSON print should not be available in the selected {:?} execution mode.",
                        &GLOBAL_CONFIG.exec_mode
                    );
                }
            };

            return res;
        }

        match &GLOBAL_CONFIG.print_mode {
            PrintMode::RawNewline | PrintMode::RawZero => self
                .values()
                .flatten()
                .map(|value| {
                    let delimiter = delimiter();
                    format!("{value}{delimiter}")
                })
                .collect::<String>(),
            PrintMode::FormattedDefault | PrintMode::FormattedNotPretty => self.format(),
        }
    }
}

impl PrintAsMap {
    pub fn to_json(&self) -> String {
        let res = match GLOBAL_CONFIG.print_mode {
            PrintMode::FormattedNotPretty | PrintMode::RawNewline | PrintMode::RawZero => {
                serde_json::to_string(&self)
            }
            PrintMode::FormattedDefault => serde_json::to_string_pretty(&self),
        };

        match res {
            Ok(s) => {
                let delimiter = delimiter();
                format!("{s}{delimiter}")
            }
            Err(error) => {
                eprintln!("Error: {error}");
                std::process::exit(1)
            }
        }
    }

    pub fn format(&self) -> String {
        let padding = self.map_padding();

        let write_out_buffer = self
            .iter()
            .filter(|(_key, values)| {
                if GLOBAL_CONFIG.opt_last_snap.is_some() {
                    !values.is_empty()
                } else {
                    true
                }
            })
            .map(|(key, values)| {
                let display_path =
                    if matches!(&GLOBAL_CONFIG.print_mode, PrintMode::FormattedNotPretty) {
                        key.clone()
                    } else {
                        format!("\"{key}\"")
                    };

                let values_string: String = values
                    .iter()
                    .enumerate()
                    .map(|(idx, value)| {
                        if matches!(&GLOBAL_CONFIG.print_mode, PrintMode::FormattedNotPretty) {
                            format!("{NOT_SO_PRETTY_FIXED_WIDTH_PADDING}{value}")
                        } else if idx == 0 {
                            format!(
                                "{:<width$} : \"{}\"\n",
                                display_path,
                                value,
                                width = padding
                            )
                        } else {
                            format!("{:<padding$} : \"{value}\"\n", "")
                        }
                    })
                    .collect::<String>();

                if matches!(&GLOBAL_CONFIG.print_mode, PrintMode::FormattedNotPretty) {
                    format!("{display_path}:{values_string}\n")
                } else {
                    values_string
                }
            })
            .collect();

        write_out_buffer
    }
}
