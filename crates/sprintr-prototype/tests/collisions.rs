// SPDX-License-Identifier: Apache-2.0

//! Collisions are process-wide, so they are isolated in this test binary.

mod common;

use common::{FailNth, assert_startup, lines, replayed, started_logger, template};
use sprintr_prototype::{Argument, Logger, Message, attributes, info};

attributes! {
    /// Account name
    account_name: str = "account.name";
}

mod other {
    sprintr_prototype::attributes! {
        /// Legal name of the account holder
        pub account_name: str = "account.name";
        /// Worker identifier
        pub worker: str = "process.pid";
    }
}

mod levels {
    sprintr_prototype::entities! {
        /// Logging level
        pub same = "log.level" {
            /// Informational
            INFO,
        }
        /// Severity
        pub different = "log.level";
    }
}

/// The `[kind, declared, description, recorded]` arguments of a collision warning.
fn warning(record: &Message) -> [String; 4] {
    record
        .args
        .iter()
        .map(|argument| match argument {
            Argument::String(text) => text.clone(),
            _ => panic!("string argument"),
        })
        .collect::<Vec<_>>()
        .try_into()
        .unwrap()
}

fn suffixed(recorded: &str, declared: &str) -> bool {
    recorded
        .strip_prefix(declared)
        .and_then(|rest| rest.strip_prefix('_'))
        .is_some_and(|hash| {
            hash.len() == 8
                && hash
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
}

/// Scenario: declarations disagree with sprintr's schema and with each other.
/// Guarantees: logging still starts; sprintr's names win, the first user declaration keeps its
/// name, later ones are renamed, and every rename is announced after startup in each stream.
#[test]
fn collisions_are_renamed_and_announced() {
    let _ = (&levels::same, &levels::different);
    let mut logger = started_logger();
    logger.with_default(|| {
        info!(
            "{} {}",
            account_name = "Ada",
            other::account_name = "Ada Lovelace"
        );
        info!("{}", other::worker = "worker-7");
    });
    let encoded = logger.into_inner();
    let records = lines(&encoded);
    assert_startup(&records[0]);
    assert_eq!(
        template(&records[1]).msg,
        "{kind} {declared} ({description}) collides with an earlier declaration; recorded as {recorded}"
    );
    assert_eq!(
        serde_json::to_value(&template(&records[1]).attrs).unwrap(),
        serde_json::json!({"log.level": "WARN"})
    );
    let statement = records[1].attrs["log.statement"];
    assert!(
        records[1..5]
            .iter()
            .all(|record| record.attrs["log.statement"] == statement)
    );
    let mut warnings: Vec<_> = records[1..5].iter().map(warning).collect();
    warnings.sort();
    let [account, pid, level, info] = &warnings[..] else {
        unreachable!()
    };

    assert_eq!(account[..2], ["attribute", "account.name"]);
    assert!(suffixed(&account[3], "account.name"), "{account:?}");
    assert_eq!(pid[..3], ["attribute", "process.pid", "Worker identifier"]);
    assert!(suffixed(&pid[3], "process.pid"), "{pid:?}");
    assert_eq!(
        info[..3],
        ["named entity", "log.level:INFO", "Informational"]
    );
    assert!(suffixed(&info[3], "log.level:INFO"), "{info:?}");
    assert_eq!(level[..3], ["entity", "log.level", "Severity"]);
    assert!(suffixed(&level[3], "log.level"), "{level:?}");

    // Link order decides which user declaration of a name is first.
    let names = if account[2] == "Account name" {
        [account[3].as_str(), "account.name"]
    } else {
        assert_eq!(account[2], "Legal name of the account holder");
        ["account.name", account[3].as_str()]
    };
    let statement = &records[5];
    assert_eq!(
        serde_json::to_value(&template(statement).refs).unwrap(),
        serde_json::json!([{names[0]: {"$arg": 0}}, {names[1]: {"$arg": 1}}])
    );
    assert_eq!(
        template(statement).msg,
        format!("{{@{}}} {{@{}}}", names[0], names[1])
    );
    assert!(
        statement
            .defs
            .contains_key(&format!("log.relation:{}", account[3]))
    );
    let worker = &records[6];
    assert_eq!(template(worker).msg, format!("{{@{}}}", pid[3]));
    assert_eq!(
        serde_json::to_value(&worker.defs[&format!("log.relation:{}", pid[3])]).unwrap(),
        serde_json::json!({"kind": "attribute", "type": "string", "desc": "Worker identifier"})
    );

    let announced: String = records[1..5]
        .iter()
        .map(|record| {
            let [kind, declared, description, recorded] = warning(record);
            format!(
                "{kind} {declared} ({description}) collides with an earlier declaration; \
                 recorded as {recorded}\nlog.level = WARN\n"
            )
        })
        .collect();
    let text = replayed(&encoded);
    assert_eq!(
        text.strip_prefix(&announced).unwrap(),
        format!(
            "Ada Ada Lovelace\nlog.level = INFO, {} = Ada, {} = Ada Lovelace\n\
             worker-7\nlog.level = INFO, {} = worker-7\n",
            names[0], names[1], pid[3]
        )
    );

    let mut another = started_logger();
    another.with_default(|| info!("again"));
    another.start().unwrap();
    let again = another.into_inner();
    for (later, first) in lines(&again).iter().zip(&records).take(5) {
        assert_eq!(later.defs, first.defs);
        assert_eq!(later.args, first.args);
        assert_eq!(later.attrs["log.statement"], first.attrs["log.statement"]);
        assert!(later.attrs["log.record"] > first.attrs["log.record"]);
    }

    assert_eq!(
        replayed(&again),
        format!("{announced}again\nlog.level = INFO\n")
    );
}

/// The sink accepts the startup record and three collision warnings, then rejects the fourth.
#[test]
fn partial_startup_failure_is_not_reannounced() {
    let mut logger = Logger::new(FailNth::new(5));
    let failure = logger.start().unwrap_err();
    assert_eq!(failure.kind(), std::io::ErrorKind::BrokenPipe);
    logger.with_default(|| info!("after partial startup"));
    let repeated = logger.start().unwrap_err();
    assert_eq!(repeated.kind(), failure.kind());
    assert_eq!(repeated.to_string(), failure.to_string());

    let sink = logger.into_inner();
    assert_eq!(sink.writes, 6);
    let text = replayed(&sink.output);
    assert_eq!(
        text.matches("collides with an earlier declaration").count(),
        3
    );
    assert!(text.ends_with("after partial startup\nlog.level = INFO\n"));
}
