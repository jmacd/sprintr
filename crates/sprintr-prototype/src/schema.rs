// SPDX-License-Identifier: Apache-2.0

//! The relations sprintr itself records.
//!
//! These are declared like any user relation. They are resolved before other
//! declarations, so a conflicting user declaration is the one renamed.

// Registration expands to linkme `#[link_section]` statics through a helper
// macro defined in this crate, so the lint applies here but not in user crates.
#![allow(unsafe_code)]

use crate::__private::Declaration;

crate::entities! {
    /// Logging statement
    #[anonymous]
    pub log_statement = "log.statement";

    /// Native source location identified by its program counter in this process
    #[anonymous]
    pub log_source = "log.source";

    /// Logging thread identified by its native operating-system thread ID
    #[anonymous]
    pub log_thread = "log.thread";

    /// The record itself, identified by its zero-based sequence on log.thread
    #[anonymous]
    pub log_record = "log.record";

    /// One captured failure, identified by its generated process-local UID
    #[anonymous]
    pub exception = "exception";
    /// Rust variant classifying an exception statement
    #[named(global)]
    pub exception_class = "exception.class";

    /// Rust type name for a structured object argument
    #[named(global)]
    pub struct_name = "struct.name";

    /// Named encoder for a positional structured argument
    #[named(global)]
    pub struct_encode = "struct.encode";

    /// Start record of this span, named by its 16-digit span ID
    #[named(within = trace_start, id = span_id)]
    pub span_start = "span.start";
    /// Root start record of this trace, named by its 32-digit trace ID
    #[named(global, id = trace_id)]
    pub trace_start = "trace.start";
    /// Parent span's start record at child creation
    #[named(within = trace_start, id = span_id)]
    pub span_parent = "span.parent";

    /// Purpose of a statement in the event stream
    #[named(global)]
    pub log_kind = "log.kind" {
        /// Creation of a detached instrumentation context
        CONTEXT_START,
        /// Start of a span
        SPAN_START,
        /// End of a span
        SPAN_END,
        /// Capture of a structured exception occurrence
        EXCEPTION,
    }

    /// Logging level
    #[named(global)]
    pub log_level = "log.level" {
        /// Informational event
        INFO,
        /// Warning condition
        WARN,
        /// Error condition
        ERROR,
    }
}

crate::attributes! {
    /// Prior exception occurrence that caused this failure
    pub exception_cause: entity(exception) = "exception.cause";
    /// OTel trace ID in canonical lowercase hexadecimal
    pub trace_id: str = "trace.id";
    /// OTel span ID in canonical lowercase hexadecimal, local to its trace
    pub span_id: str = "span.id";
    /// Caller PCs after the statement's static frame zero, innermost first; each u64 identifies a log.source
    pub log_stack: [u64] = "log.stack";
    /// Process identifier (PID)
    pub process_pid: u64 = "process.pid";
    /// Name of the telemetry SDK
    pub telemetry_sdk_name: str = "telemetry.sdk.name";
    /// Version of the telemetry SDK
    pub telemetry_sdk_version: str = "telemetry.sdk.version";
}

/// Resolution order for sprintr's own declarations; not a reservation.
pub(crate) static DECLARATIONS: [Declaration; 27] = [
    Declaration::Relation(exception.__meta()),
    Declaration::Relation(exception_class.__meta()),
    Declaration::Relation(exception_cause.__meta()),
    Declaration::Named(&log_kind::EXCEPTION),
    Declaration::Relation(trace_id.__meta()),
    Declaration::Relation(span_id.__meta()),
    Declaration::Relation(span_start.__meta()),
    Declaration::Relation(trace_start.__meta()),
    Declaration::Relation(span_parent.__meta()),
    Declaration::Named(&log_kind::SPAN_START),
    Declaration::Named(&log_kind::SPAN_END),
    Declaration::Relation(log_kind.__meta()),
    Declaration::Named(&log_kind::CONTEXT_START),
    Declaration::Relation(struct_name.__meta()),
    Declaration::Relation(struct_encode.__meta()),
    Declaration::Relation(log_statement.__meta()),
    Declaration::Relation(log_stack.__meta()),
    Declaration::Relation(log_source.__meta()),
    Declaration::Relation(log_thread.__meta()),
    Declaration::Relation(log_record.__meta()),
    Declaration::Relation(log_level.__meta()),
    Declaration::Named(&log_level::INFO),
    Declaration::Named(&log_level::WARN),
    Declaration::Named(&log_level::ERROR),
    Declaration::Relation(process_pid.__meta()),
    Declaration::Relation(telemetry_sdk_name.__meta()),
    Declaration::Relation(telemetry_sdk_version.__meta()),
];
