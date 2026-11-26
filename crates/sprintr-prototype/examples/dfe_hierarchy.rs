// SPDX-License-Identifier: Apache-2.0

//! Two workers in one simulated engine process a batch each. Both pipelines
//! have a node called `batch`; their containing pipelines distinguish them.
//! Pass `--fail-metrics` to close the metrics output and record its cause chain.

use serde_json::json;
use sprintr_prototype::{
    EntityHandle, StructuredError, attributes, entities, exception, info, span, start, warn,
};
use std::{io, thread};
use thiserror::Error;

attributes! {
    /// Simulated engine incarnation
    engine_uid: str = "engine.uid";
    /// Group name within the engine
    group_id: str = "pipeline_group.id";
    /// Pipeline name within the group
    pipeline_id: str = "pipeline.id";
    /// Node name within the pipeline
    node_id: str = "node.id";
    /// Input batch number
    batch_id: u64 = "batch.id";
}

entities! {
    /// Engine
    #[named(global, id = engine_uid)]
    engine = "engine";
    /// Pipeline group
    #[named(within = engine, id = group_id)]
    group = "pipeline_group";
    /// Pipeline
    #[named(within = group, id = pipeline_id)]
    pipeline = "pipeline";
    /// Processing node
    #[named(within = pipeline, id = node_id)]
    node = "node";
}

#[derive(Debug, Error)]
enum NodeError {
    #[error(transparent)]
    OutputClosed(StructuredError),
}

#[derive(Debug, Error)]
enum PipelineError {
    #[error(transparent)]
    BatchRejected(StructuredError),
}

fn forward(batcher: &EntityHandle, batch: u64, output_open: bool) -> Result<(), NodeError> {
    if !output_open {
        return Err(exception!(
            NodeError::OutputClosed,
            "output of {batcher} is closed"
        ));
    }
    info!("forwarded batch {}", batch_id = batch);
    Ok(())
}

fn process_batch(signal: &str, batch: u64, output_open: bool) -> io::Result<()> {
    // Both workers describe the same fixture engine and group using local handles.
    let instance = engine.named(
        "0193a1c2-02ac-7000-8000-000000000001",
        json!({"name":"demo"}),
    )?;
    let ingest = group
        .within(&instance)
        .named("ingest", json!({"policy":"per-signal"}))?;
    let flow = pipeline
        .within(&ingest)
        .named(signal, json!({"signal":signal}))?;
    let batcher = node
        .within(&flow)
        .named("batch", json!({"kind":"processor"}))?;

    // A trace describes this execution, independently of the entity lifetimes.
    span!("dispatch batch {} on {instance}", batch_id = batch).in_scope(|| {
        let result = span!("route through {ingest}").in_scope(|| {
            span!("run {flow}").in_scope(|| {
                let sent =
                    span!("execute {batcher}").in_scope(|| forward(&batcher, batch, output_open));
                sent.map_err(|NodeError::OutputClosed(inner)| {
                    exception!(
                        PipelineError::BatchRejected,
                        "{flow} rejected batch {}",
                        batch_id = batch,
                        cause = &inner
                    )
                })
            })
        });
        if let Err(PipelineError::BatchRejected(failure)) = result {
            warn!("dropping batch: {failure}");
        }
    });
    Ok(())
}

fn main() -> io::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let usage = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: dfe_hierarchy [--fail-metrics] [--symbols MODULE.gsym]...",
        )
    };
    let mut metrics_open = true;
    let mut packs = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "--fail-metrics" && metrics_open {
            metrics_open = false;
        } else if arg == "--symbols" {
            packs.push(args.next().ok_or_else(usage)?);
        } else {
            return Err(usage());
        }
    }
    start()?;
    if !packs.is_empty() {
        sprintr_prototype::symbols::record_symbols(packs)?;
    }
    thread::scope(|scope| {
        let logs = scope.spawn(|| process_batch("logs", 1, true));
        process_batch("metrics", 2, metrics_open)?;
        logs.join().expect("logs worker panicked")
    })
}
