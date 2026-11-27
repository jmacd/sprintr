// SPDX-License-Identifier: Apache-2.0

use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Command, Output},
};
use tempfile::NamedTempFile;

fn site(text: &str, line: u32) -> Value {
    json!({"source":{"mod":"app","file":"src/main.rs","line":line,"column":3},
        "descriptor":{"kind":"statement","source_format":text,"recorded_format":text.replace("{}","{@job.id}"),"attrs":{"log.level":"WARN"},
        "arguments":[{"label":"@job.id","attribute":"job.id","capture":"attribute","wire_type":"string","specifier":""}],"has_cause":false}})
}

fn catalog(sites: Vec<Value>) -> Value {
    json!({"format":"sprintr.catalog/1","relations":{},"named_values":{},"sites":sites})
}

fn report(a: &Value, b: &Value, fmt: &str) -> Output {
    let temp = |v: &Value| {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "{v}").unwrap();
        f
    };
    let a = temp(a);
    let b = temp(b);
    Command::new(env!("CARGO_BIN_EXE_sprintr"))
        .args(["schema", "diff"])
        .arg(a.path())
        .arg(b.path())
        .args(["--format", fmt])
        .output()
        .unwrap()
}

/// Scenario: a line moves, a slot is rebound, and WARN becomes INFO.
/// Guarantees: report both field changes, independent of coordinates; no policy exit status.
#[test]
fn reports_level_and_binding_changes_independently() {
    let base = catalog(vec![site("dropping {}", 7)]);
    let mut head = catalog(vec![site("dropping {}", 47)]);
    let d = &mut head["sites"][0]["descriptor"];
    d["attrs"]["log.level"] = "INFO".into();
    d["arguments"][0]["attribute"] = "job.parent".into();
    d["arguments"][0]["label"] = "@job.parent".into();
    d["recorded_format"] = "dropping {@job.parent}".into();
    let out = report(&base, &head, "json");
    assert!(out.status.success(), "{:?}", out.stderr);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let changes = v["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["change"], "modified");
    let fields = changes[0]["fields"].as_array().unwrap();
    assert!(fields.contains(&json!({"path":"/attrs/log.level","before":"WARN","after":"INFO"})));
    assert!(fields.contains(
        &json!({"path":"/arguments/0/attribute","before":"job.id","after":"job.parent"})
    ));
    let out = report(&base, &head, "markdown");
    assert!(
        out.status.success()
            && String::from_utf8_lossy(&out.stdout).contains("**modified statement**")
    );
}

/// Scenario: equal unnamed callsites move; two remaining sites change wording.
/// Guarantees: moves are silent, multiplicity survives, ambiguous edits are never paired.
#[test]
fn preserves_duplicates_and_refuses_ambiguous_pairs() {
    let base = catalog(vec![
        site("same {}", 1),
        site("same {}", 2),
        site("first {}", 3),
    ]);
    let moved = catalog(vec![
        site("same {}", 100),
        site("first {}", 101),
        site("same {}", 102),
    ]);
    let report_json = |head: &Value| {
        let out = report(&base, head, "json");
        assert!(out.status.success());
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["changes"]
            .as_array()
            .unwrap()
            .clone()
    };
    assert!(report_json(&moved).is_empty());
    let head = catalog(vec![
        site("same {}", 2),
        site("also {}", 3),
        site("more {}", 4),
    ]);
    let changes = report_json(&head);
    assert_eq!(changes.iter().filter(|c| c["change"] == "added").count(), 2);
    assert_eq!(
        changes.iter().filter(|c| c["change"] == "removed").count(),
        2
    );
    assert_eq!(changes.len(), 4);
    let one_edit = catalog(vec![
        site("same {}", 1),
        site("same {}", 2),
        site("new wording {}", 7),
    ]);
    assert_eq!(report_json(&one_edit)[0]["change"], "modified");
}
