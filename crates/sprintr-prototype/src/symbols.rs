// SPDX-License-Identifier: Apache-2.0

//! Reader-side symbolization. Neither unwinding nor writing a record loads this map.

use findshlibs::{Segment, SharedLibrary, TargetSharedLibrary};
use serde::{Deserialize, Serialize};
use sprintr_message::short_path;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io;

/// One executable segment as mapped in the originating process. `linked` is
/// the same segment's address in the binary, before relocation.
#[derive(Debug, Serialize, Deserialize)]
pub struct CodeSegment {
    pub loaded: u64,
    pub linked: u64,
    pub size: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_stack_names_hide_type_arguments() {
        for symbol in [
            "call_once<fn() -> core::result::Result<(), alloc::boxed::Box<Error>>, ()>",
            "<F as core::ops::FnOnce<()>>::call_once",
            "core::ops::function::call_once::<pkg::T>",
        ] {
            assert_eq!(short_function(symbol), "call_once");
        }
    }

    #[test]
    fn frames_use_at_syntax_and_whole_source_path_components() {
        let loc = SourceLocation {
            function: Some("core::ops::call_once::<pkg::T>".into()),
            file: Some("/toolchain/src/rust/library/core/src/ops/function.rs".into()),
            line: Some(250),
            module: None,
        };
        assert_eq!(
            format_frame(1, Some(&loc)),
            "at call_once (src/ops/function.rs:250)"
        );
        assert_eq!(format_frame(42, None), "at <unresolved> (0x2a)");
        assert_eq!(short_path("/work/crates/foo/lib.rs"), "crates/foo/lib.rs");
        assert_eq!(
            short_path("/work/crates/sprintr-prototype/examples/tour.rs"),
            ".../examples/tour.rs"
        );
        assert_eq!(short_path(r"C:\work\src\bin\main.rs"), r"src\bin\main.rs");
        assert_eq!(
            short_path("/work/mysrc/bin/main.rs"),
            "/work/mysrc/bin/main.rs"
        );
        let basename = "\u{e9}".repeat(32);
        assert_eq!(
            short_path(&format!("/dir/{basename}")),
            format!(".../{basename}")
        );
        let long_name = "long_filename_with_no_natural_breakpoint.rs";
        assert_eq!(short_path(long_name), long_name);
    }

    #[test]
    fn offline_map_relocates_return_pcs_without_the_original_address_space() {
        let map: ModuleMap = serde_json::from_str(r#"{"modules":[{"path":"app","debug_id":"build","segments":[{"loaded":4096,"linked":128,"size":16}]}]}"#).unwrap();
        assert_eq!(
            map.locate(4097).map(|(m, a)| (m.path.as_str(), a)),
            Some(("app", 128))
        );
        assert_eq!(map.locate(4112).unwrap().1, 143);
        assert!(map.locate(4096).is_none());
        assert!(map.locate(4113).is_none());
        assert!(map.locate(0).is_none());
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Module {
    pub path: String,
    pub debug_id: Option<String>,
    pub segments: Vec<CodeSegment>,
}

/// Bulk sidecar for one execution. It contains no per-PC symbol records.
#[derive(Debug, Serialize, Deserialize)]
pub struct ModuleMap {
    pub modules: Vec<Module>,
}

impl ModuleMap {
    /// Snapshot after recording, while the originating images are still loaded.
    pub fn current_process() -> io::Result<Self> {
        let exe = std::env::current_exe()?;
        let mut modules = Vec::new();
        TargetSharedLibrary::each(|image| {
            let segments: Vec<_> = image
                .segments()
                .filter(|s| s.is_code())
                .map(|s| CodeSegment {
                    loaded: s.actual_virtual_memory_address(image).0 as u64,
                    linked: s.stated_virtual_memory_address().0 as u64,
                    size: s.len() as u64,
                })
                .collect();
            if !segments.is_empty() {
                let path = if image.name().is_empty() {
                    exe.as_os_str()
                } else {
                    image.name()
                };
                modules.push(Module {
                    path: path.to_string_lossy().into_owned(),
                    debug_id: image.debug_id().map(|id| id.to_string()),
                    segments,
                });
            }
        });
        if modules.is_empty() {
            return Err(io::Error::other(
                "no executable segments available from the loader",
            ));
        }
        Ok(Self { modules })
    }

    /// Translate a raw return PC to the module and linked **call instruction**
    /// address to give an offline symbolizer. Never consult the replay process.
    pub fn locate(&self, pc: u64) -> Option<(&Module, u64)> {
        let instruction = pc.checked_sub(1)?;
        self.modules.iter().find_map(|module| {
            module.segments.iter().find_map(|segment| {
                let offset = instruction.checked_sub(segment.loaded)?;
                if offset >= segment.size {
                    return None;
                }
                Some((module, segment.linked.checked_add(offset)?))
            })
        })
    }
}

/// Best-effort native debug information for one physical or inlined frame.
#[derive(Debug)]
pub struct SourceLocation {
    pub module: Option<String>,
    pub function: Option<String>,
    pub file: Option<std::path::PathBuf>,
    pub line: Option<u32>,
}

fn short_function(name: &str) -> &str {
    crate::rust_leaf_name(name).unwrap_or("<unknown function>")
}

/// Local lookup is **only** for PCs from this execution. Cache misses as well
/// as hits; a stripped binary may have no name or line information.
pub struct LocalSymbols {
    modules: ModuleMap,
    cache: HashMap<u64, Vec<SourceLocation>>,
}

impl LocalSymbols {
    pub fn current_process() -> io::Result<Self> {
        Ok(Self {
            modules: ModuleMap::current_process()?,
            cache: HashMap::new(),
        })
    }

    pub fn resolve(&mut self, pc: u64) -> &[SourceLocation] {
        self.cache.entry(pc).or_insert_with(|| {
            let mut locations = Vec::new();
            let module = self.modules.locate(pc).map(|(m, _)| m.path.as_str());
            if let Ok(ip) = usize::try_from(pc)
                && ip != 0
            {
                backtrace::resolve(std::ptr::without_provenance_mut(ip), |symbol| {
                    locations.push(SourceLocation {
                        module: module.map(str::to_owned),
                        function: symbol.name().map(|n| format!("{n:#}")),
                        file: symbol.filename().map(ToOwned::to_owned),
                        line: symbol.lineno(),
                    });
                });
            }
            if locations.is_empty() && module.is_some() {
                locations.push(SourceLocation {
                    module: module.map(str::to_owned),
                    function: None,
                    file: None,
                    line: None,
                });
            }
            locations
        })
    }

    /// Compact text for one caller; the cached symbols retain their full names and paths.
    pub fn format_frame(&mut self, pc: u64) -> String {
        format_frame(pc, self.resolve(pc).first())
    }
}

fn format_frame(pc: u64, location: Option<&SourceLocation>) -> String {
    let Some(loc) = location else {
        return format!("at <unresolved> ({pc:#x})");
    };
    let mut text = match loc.function.as_deref() {
        Some(name) => format!("at {}", short_function(name)),
        None => format!("at {pc:#x}"),
    };
    if let Some(file) = &loc.file {
        write!(text, " ({}", short_path(&file.to_string_lossy())).expect("write to string");
        if let Some(line) = loc.line {
            write!(text, ":{line}").expect("write to string");
        }
        text.push(')');
    } else if let Some(module) = &loc.module {
        write!(text, " ({})", short_path(module)).expect("write to string");
    }
    text
}
