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

    /// Logging level
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
pub(crate) static DECLARATIONS: [Declaration; 12] = [
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
