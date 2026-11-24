// SPDX-License-Identifier: Apache-2.0

mod common;

use common::FailNth;
use sprintr_prototype::{Logger, buffer::StackBuffer, context, error, info, replay, warn};
use std::{env, io, process::Command};

fn global_event(text: &str) {
    error!("{text:s}")
}

fn exercise_outputs() {
    // Delimit stdout so the parent can separate JSON Lines from libtest's own output.
    println!("\nBEGIN OUTPUT");
    let mut unstarted = Logger::new(Vec::new());
    unstarted.with_default(|| warn!("local {:s}", "preview"));
    assert!(unstarted.into_inner().is_empty());

    let mut logger = Logger::with_buffer(FailNth::new(2), StackBuffer::<2048>::new());
    logger.start().unwrap();
    for text in [&"x".repeat(4096)[..], "rejected by sink", "local accepted"] {
        logger.with_default(|| info!("{text:s}"));
    }
    let sink = logger.into_inner();
    assert_eq!(sink.writes, 3);
    assert_eq!(
        common::replayed(&sink.output),
        "local accepted\nlog.level = INFO\n"
    );

    // Starting a caller-owned logger did not start stdout.
    global_event("global preview");
    assert!(context!("preview context").origin().is_none());
    println!("START STDOUT");
    sprintr_prototype::start().unwrap();
    let mut scoped = common::started_logger();
    scoped.with_default(|| {
        global_event("scoped parent");
        std::thread::spawn(|| {
            sprintr_prototype::start().unwrap();
            let origin = context!("global context").origin().unwrap();
            assert_eq!(origin.thread(), gettid::gettid());
            global_event("global accepted");
        })
        .join()
        .unwrap();
    });
    assert_eq!(
        common::replayed(&scoped.into_inner()),
        "scoped parent\nlog.level = ERROR\n"
    );
    println!("END OUTPUT");
}

/// Scenario: library instrumentation runs under nested defaults, including
/// one that unwinds. Guarantees: each event goes only to the innermost scope.
#[test]
fn nested_defaults_restore_the_previous_writer() {
    let mut bytes = Vec::new();
    let mut outer = Logger::new(&mut bytes);
    outer.start().unwrap();
    let mut inner = common::started_logger();
    let result = outer.with_default(|| {
        inner.with_default(|| global_event("inner"));
        global_event("after return");
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            inner.with_default(|| {
                global_event("inner before panic");
                panic!("leave the inner scope");
            });
        }));
        assert!(unwound.is_err());
        global_event("after unwind");
        42
    });
    assert_eq!(result, 42);
    assert_eq!(
        common::replayed(&bytes),
        "after return\nlog.level = ERROR\nafter unwind\nlog.level = ERROR\n"
    );
    assert_eq!(
        common::replayed(&inner.into_inner()),
        "inner\nlog.level = ERROR\ninner before panic\nlog.level = ERROR\n"
    );
}

#[test]
fn explicit_start_and_stderr_diagnostics() {
    const CHILD: &str = "SPRINTR_TEST_STDIO_CHILD";
    if env::var_os(CHILD).is_some() {
        exercise_outputs();
        return;
    }
    let output = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            "explicit_start_and_stderr_diagnostics",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");
    for preview in ["local preview", "global preview", "preview context"] {
        assert!(stderr.contains(preview), "{stderr}");
    }
    assert_eq!(
        stderr.matches("has not been started").count(),
        3,
        "{stderr}"
    );
    assert!(
        stderr.contains(&io::ErrorKind::StorageFull.to_string()),
        "{stderr}"
    );
    assert!(stderr.contains("sink rejected record"), "{stderr}");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let (_, stream) = stdout
        .split_once("BEGIN OUTPUT\nSTART STDOUT\n")
        .expect("no stdout before start");
    let (stream, _) = stream.split_once("END OUTPUT").unwrap();
    assert_eq!(common::lines(stream.as_bytes()).len(), 3);
    let mut text = Vec::new();
    replay(stream.as_bytes(), &mut text).unwrap();
    let text = String::from_utf8(text).unwrap();
    assert!(text.contains("started logging with sprintr"));
    assert!(text.ends_with("global accepted\nlog.level = ERROR\n"));
}
