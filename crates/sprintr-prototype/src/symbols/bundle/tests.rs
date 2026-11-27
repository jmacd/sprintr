// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::{Logger, Renderer};
use serde_json::json;

// Standalone GSYM v1: one 16-byte function at linked 0x1000, file 1,
// line 42. The first line opcode emits a row at the function start.
fn fixture() -> Vec<u8> {
    let strings = b"\0forward\0src\0node.rs\0";
    let mut bytes = b"MYSG\x01\x00\x01\x10".to_vec();
    bytes.extend(0x1000u64.to_le_bytes());
    let word = |bytes: &mut Vec<u8>, n: u32| bytes.extend(n.to_le_bytes());
    word(&mut bytes, 1); // address count
    word(&mut bytes, 76); // string table offset
    word(&mut bytes, strings.len() as u32);
    bytes.extend([0xab; 16]); // UUID
    bytes.extend([0; 4]); // UUID padding
    bytes.extend([0; 4]); // address offset 0 + alignment
    word(&mut bytes, 100); // function info offset
    word(&mut bytes, 2); // file table size
    for offset in [0, 0, 9, 13] {
        word(&mut bytes, offset);
    }
    bytes.extend(strings);
    bytes.resize(100, 0);
    for field in [16, 1, 1, 5] {
        word(&mut bytes, field);
    }
    bytes.extend([0, 0, 42, 4, 0]); // line header and opcodes
    bytes.extend([0; 8]); // end of info list
    bytes
}

fn bundle() -> Bundle {
    let map = serde_json::from_value(json!({"modules":[
        {"path":"/absent/app", "debug_id":"abababab-abab-abab-abab-abababababab", "segments":[{"loaded": 0x8000, "linked": 0x1000, "size": 0x100}]},
        {"path":"/absent/lib", "debug_id":"beef", "segments":[{"loaded": 0x9000, "linked": 0x300, "size": 0x10}]}
    ]})).unwrap();
    let raw = fixture();
    let packs = [(format!("{:x}", Sha256::digest(&raw)), STANDARD.encode(raw))].into();
    Bundle {
        format: Format::GsymV1,
        arch: "aarch64".into(),
        map,
        packs,
    }
}

#[allow(unsafe_code)] // linkme callsite registration
fn recording() -> Vec<Message> {
    let mut logger = Logger::new(Vec::new());
    logger.start().unwrap();
    logger.with_default(|| {
        publish(bundle()).unwrap();
        crate::info!("batch finished");
    });
    serde_json::Deserializer::from_slice(&logger.into_inner())
        .into_iter::<Message>()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Scenario: replay a relocated process after the executable and pack are gone.
/// Guarantees: the JSON Lines alone give the line for a previously unseen PC,
/// and gaps, missing library packs, and unmapped PCs remain distinguishable.
#[test]
fn recording_alone_resolves_stack_frames() {
    let mut renderer = Renderer::default();
    let mut symbols = RecordedSymbols::default();
    let mut publications = 0;
    for event in recording() {
        let text = renderer.render(&event).unwrap();
        if event.context.contains_key("log.recorded_symbols") {
            publications += 1;
            assert!(!text.contains(&STANDARD.encode(fixture())));
        }
        symbols.observe(&event).unwrap();
    }
    assert_eq!(publications, 1);
    assert_eq!(
        symbols.format_frame(0x8010).unwrap().as_deref(),
        Some("at forward (src/node.rs:42)")
    );
    assert!(
        symbols
            .format_frame(0x8011)
            .unwrap()
            .unwrap()
            .contains("no symbol")
    );
    let missing = symbols.format_frame(0x9001).unwrap().unwrap();
    assert!(missing.contains("0x9001") && missing.contains("[beef] @ 0x300; no symbol pack"));
    assert!(symbols.format_frame(0x8000).unwrap().is_none());
}

/// Scenario: a corrupt or wrong-build bundle precedes a valid publication.
/// Guarantees: a failed installation leaves no map, digest receipt, or symbols behind.
#[test]
fn rejects_mismatched_data_transactionally() {
    let event = recording()
        .into_iter()
        .find(|e| e.context.contains_key("log.recorded_symbols"))
        .unwrap();
    let key = &event.context["log.recorded_symbols"].entity;
    let mut symbols = RecordedSymbols::default();
    for wrong_build in [false, true] {
        let mut bad = event.clone();
        let Some(Definition::Entity(obj)) = bad.defs.get_mut(key) else {
            unreachable!()
        };
        if wrong_build {
            obj.get_mut("map").unwrap()["modules"][0]["debug_id"] = "cd".into();
        } else {
            let pack = obj
                .get_mut("packs")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .values_mut()
                .next()
                .unwrap();
            *pack = STANDARD.encode(b"corrupt").into();
        }
        assert!(symbols.observe(&bad).is_err());
        assert!(symbols.format_frame(0x8001).unwrap().is_none());
    }
    symbols.observe(&event).unwrap();
    assert!(
        symbols
            .format_frame(0x8001)
            .unwrap()
            .unwrap()
            .contains("forward")
    );
}
