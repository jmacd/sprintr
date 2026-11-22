// SPDX-License-Identifier: Apache-2.0

// An isolated test binary: the capture limit is process-wide.
mod common;

use sprintr_prototype::{Logger, Renderer, info, set_stack_limit};

#[test]
fn static_invocation_survives_disabling_caller_capture() {
    #[inline(never)]
    fn emit(logger: &mut Logger<Vec<u8>>) -> u32 {
        let line = line!() + 1;
        info!("one callsite", logger: logger).unwrap();
        line
    }

    set_stack_limit(0).unwrap();
    let mut logger = Logger::new(Vec::new());
    let line = emit(&mut logger);
    set_stack_limit(1).unwrap();
    emit(&mut logger);
    let records = common::lines(&logger.into_inner());
    let mut reader = Renderer::default();
    for record in &records {
        reader.render(record).unwrap();
    }
    assert!(records[0].stack.is_empty()); // startup also honors zero
    assert!(records[1].stack.is_empty());
    let source = &common::template(&records[1]).source;
    assert_eq!(source.module, module_path!());
    assert_eq!(source.file, file!());
    assert_eq!(source.line, line);
    assert_eq!(source.column, 9); // the 'i' in info!, not the format literal
    assert_eq!(records[2].stack.len(), 1);
    assert!(records[2].defs.is_empty());
    assert_eq!(
        reader.statement_source(records[2].attrs["log.statement"]),
        Some(source)
    );
}
