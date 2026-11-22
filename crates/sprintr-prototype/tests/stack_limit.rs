// SPDX-License-Identifier: Apache-2.0

// An isolated test binary: the capture limit is process-wide.
mod common;

use sprintr_prototype::{Logger, Renderer, info, message, set_stack_limit};

#[test]
fn static_invocation_survives_disabling_caller_capture() {
    #[inline(never)]
    fn emit(logger: &mut Logger<Vec<u8>>) {
        info!("one callsite", logger: logger).unwrap();
    }

    set_stack_limit(0).unwrap();
    let line = line!() + 1;
    let snapshot = message!("snapshot with no callers");
    let source = &common::template(&snapshot).source;
    assert_eq!(source.module, module_path!());
    assert_eq!(source.file, file!());
    assert_eq!(source.line, line);
    assert_eq!(source.column, 20); // the 'm' in message!, not the format literal
    assert!(snapshot.stack.is_empty());
    snapshot.validate().unwrap();

    let mut logger = Logger::new(Vec::new());
    emit(&mut logger);
    set_stack_limit(1).unwrap();
    emit(&mut logger);
    logger.write(&snapshot).unwrap();
    let records = common::lines(&logger.into_inner());
    let mut reader = Renderer::default();
    for record in &records {
        reader.render(record).unwrap();
    }
    assert!(records[0].stack.is_empty()); // startup also honors zero
    assert!(records[1].stack.is_empty());
    assert_eq!(records[2].stack.len(), 1);
    assert!(records[2].defs.is_empty());
    assert_eq!(
        reader.statement_source(records[2].attrs["log.statement"]),
        Some(&common::template(&records[1]).source)
    );
    assert!(records[3].stack.is_empty()); // writing never recaptures a snapshot
}
