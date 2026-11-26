// SPDX-License-Identifier: Apache-2.0

use super::{ModuleMap, SourceLocation, format_frame};
use crate::{Definition, Message, RecordAddress, schema};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use blazesym::symbolize::{CodeInfo, Input, Symbolized, Symbolizer, source::GsymData};
use serde::{Deserialize, Serialize, de::value::MapDeserializer};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt::Write as _, fs, io, path::Path};

#[cfg(all(test, target_endian = "little"))]
mod tests;

fn invalid(s: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, s.to_string())
}

/// A GSYM v1 header carries the original ELF build ID or Mach-O UUID, not
/// the digest of the converted pack. Match the former to the loaded image.
fn build_id(data: &[u8]) -> io::Result<String> {
    if data.len() < 48 || &data[..6] != b"MYSG\x01\x00" || !cfg!(target_endian = "little") {
        return Err(invalid("expected standalone little-endian GSYM version 1"));
    }
    if !matches!(data[6], 1 | 2 | 4 | 8) || !(1..=20).contains(&data[7]) {
        return Err(invalid("invalid GSYM address width or missing build ID"));
    }
    let id = &data[28..28 + usize::from(data[7])];
    if id.iter().all(|&b| b == 0) {
        return Err(invalid("zero GSYM build ID"));
    }
    let mut text = String::new();
    for byte in id {
        write!(text, "{byte:02x}").expect("write to string");
    }
    Ok(text)
}

#[derive(Serialize, Deserialize)]
enum Format {
    #[serde(rename = "gsym-v1")]
    GsymV1,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    format: Format,
    arch: String,
    map: ModuleMap,
    /// SHA-256 of raw bytes -> base64 (JSON transport only).
    packs: BTreeMap<String, String>,
}

fn normalize_map(map: &mut ModuleMap) -> io::Result<()> {
    let mut ranges = Vec::new();
    for m in &mut map.modules {
        if let Some(id) = &mut m.debug_id {
            // findshlibs prints Mach-O UUIDs with dashes and ELF build IDs without.
            id.retain(|c| c != '-');
            id.make_ascii_lowercase();
        }
        for s in &m.segments {
            let end = s
                .loaded
                .checked_add(s.size)
                .ok_or_else(|| invalid("loaded segment overflows"))?;
            if s.size == 0 || s.linked.checked_add(s.size).is_none() {
                return Err(invalid("invalid linked code segment"));
            }
            ranges.push((s.loaded, end));
        }
    }
    ranges.sort_unstable();
    if ranges.is_empty() || ranges.windows(2).any(|p| p[0].1 > p[1].0) {
        return Err(invalid("empty or overlapping executable map"));
    }
    Ok(())
}

fn check_pack(map: &ModuleMap, bytes: &[u8], symbolizer: &Symbolizer) -> io::Result<String> {
    let id = build_id(bytes)?;
    if !map
        .modules
        .iter()
        .any(|m| m.debug_id.as_deref() == Some(&id))
    {
        return Err(invalid(format!(
            "GSYM build ID {id} does not match any mapped image"
        )));
    }
    // Validate the top-level tables without resolving a single frame.
    symbolizer
        .symbolize(&GsymData::new(bytes).into(), Input::VirtOffset(&[]))
        .map_err(invalid)?;
    Ok(id)
}

/// Publish one bulk definition to the *started* current output, before starting
/// workers. Each input is a standalone GSYM pack for a currently loaded image.
/// The returned address signifies the sink accepted the complete record.
pub fn record_symbols(
    paths: impl IntoIterator<Item = impl AsRef<Path>>,
) -> io::Result<RecordAddress> {
    let mut map = ModuleMap::current_process()?;
    normalize_map(&mut map)?;
    let symbolizer = Symbolizer::new();
    let mut builds = BTreeMap::new();
    let mut packs = BTreeMap::new();
    for path in paths {
        let path = path.as_ref();
        let data = fs::read(path)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
        let id = check_pack(&map, &data, &symbolizer)
            .map_err(|e| invalid(format!("{}: {e}", path.display())))?;
        let digest = format!("{:x}", Sha256::digest(&data));
        if let Some(prev) = builds.insert(id.clone(), digest.clone())
            && prev != digest
        {
            return Err(invalid(format!("conflicting symbol packs for {id}")));
        }
        packs.entry(digest).or_insert_with(|| STANDARD.encode(data));
    }
    if packs.is_empty() {
        return Err(invalid("expected at least one GSYM pack"));
    }
    let bundle = Bundle {
        format: Format::GsymV1,
        arch: std::env::consts::ARCH.into(),
        map,
        packs,
    };
    publish(bundle)
}

fn publish(bundle: Bundle) -> io::Result<RecordAddress> {
    let definition = schema::symbol_bundle.new(bundle)?;
    schema::recorded_symbols.context(&definition)?.in_scope(|| {
        crate::__record!(crate; [crate::schema::log_level::INFO]; "published offline stack symbols")
    })?.ok_or_else(|| io::Error::other("symbol publication requires a started output"))
}

/// Symbol data recovered from a single process recording with stable mappings.
/// Recorded image names are diagnostic strings, never paths opened by replay.
#[derive(Default)]
pub struct RecordedSymbols {
    map: Option<ModuleMap>,
    by_build_id: BTreeMap<String, Vec<u8>>,
    symbolizer: Symbolizer,
}

impl RecordedSymbols {
    /// Observe a record already accepted by `Renderer`. Installation is atomic:
    /// a wrong digest, mismatched build, or invalid table installs nothing.
    pub fn observe(&mut self, event: &Message) -> io::Result<()> {
        let Some(binding) = event.context.get(schema::recorded_symbols.name()) else {
            return Ok(());
        };
        if self.map.is_some() {
            return Err(invalid("multiple symbol maps in one recording"));
        }
        let Some(Definition::Entity(obj)) = event.defs.get(&binding.entity) else {
            return Err(invalid("symbol publication requires its bundle definition"));
        };
        let fields = obj.iter().map(|(k, v)| (k.as_str(), v));
        let mut bundle = Bundle::deserialize(MapDeserializer::<_, serde_json::Error>::new(fields))
            .map_err(invalid)?;
        normalize_map(&mut bundle.map)?;
        if bundle.arch.is_empty() || bundle.packs.is_empty() {
            return Err(invalid("missing symbol pack or architecture"));
        }
        let mut packs = BTreeMap::new();
        for (digest, encoded) in bundle.packs {
            let bytes = STANDARD.decode(encoded).map_err(invalid)?;
            if format!("{:x}", Sha256::digest(&bytes)) != digest {
                return Err(invalid("symbol pack SHA-256 mismatch"));
            }
            let id = check_pack(&bundle.map, &bytes, &self.symbolizer)?;
            if packs.insert(id, bytes).is_some() {
                return Err(invalid("multiple symbol packs for one build ID"));
            }
        }
        self.map = Some(bundle.map);
        self.by_build_id = packs;
        Ok(())
    }

    /// Render physical and inlined frames, innermost first, from embedded
    /// bytes only. Unknown PCs return `None`; missing coverage retains the PC,
    /// image ID and linked address. Malformed lookup data returns an error.
    pub fn format_frame(&self, pc: u64) -> io::Result<Option<String>> {
        let Some((module, linked)) = self.map.as_ref().and_then(|m| m.locate(pc)) else {
            return Ok(None);
        };
        let id = module.debug_id.as_deref().unwrap_or("<no build ID>");
        let fallback = |reason| {
            Some(format!(
                "at {pc:#x} ({} [{id}] @ {linked:#x}; {reason})",
                sprintr_message::short_path(&module.path)
            ))
        };
        let Some(data) = self.by_build_id.get(id) else {
            return Ok(fallback("no symbol pack"));
        };
        let sym = self
            .symbolizer
            .symbolize_single(&GsymData::new(data).into(), Input::VirtOffset(linked))
            .map_err(invalid)?;
        let Symbolized::Sym(sym) = sym else {
            return Ok(fallback("no symbol"));
        };
        let frame = |name: &str, code: Option<&CodeInfo<'_>>| {
            format_frame(
                pc,
                Some(&SourceLocation {
                    function: Some(name.into()),
                    file: code.map(|c| c.to_path().into_owned()),
                    line: code.and_then(|c| c.line),
                    module: Some(module.path.clone()),
                }),
            )
        };
        let frames: Vec<_> = sym
            .inlined
            .iter()
            .rev()
            .map(|f| frame(&f.name, f.code_info.as_ref()))
            .chain(std::iter::once_with(|| {
                frame(&sym.name, sym.code_info.as_deref())
            }))
            .collect();
        Ok(Some(frames.join("\n    ")))
    }
}
