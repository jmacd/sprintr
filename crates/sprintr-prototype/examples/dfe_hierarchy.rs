// SPDX-License-Identifier: Apache-2.0

//! Two workers in one simulated engine process a batch each. Both pipelines
//! have a node called `batch`; their containing pipelines distinguish them.

use serde_json::json;
use sprintr_prototype::{attributes, entities, info, span, start};
use std::{io, thread};

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

fn process_batch(signal: &str, batch: u64) -> io::Result<()> {
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
        span!("route through {ingest}").in_scope(|| {
            span!("run {flow}").in_scope(|| {
                span!("execute {batcher}")
                    .in_scope(|| info!("forwarded batch {}", batch_id = batch));
            });
        });
    });
    Ok(())
}

fn main() -> io::Result<()> {
    start()?;
    thread::scope(|scope| {
        let logs = scope.spawn(|| process_batch("logs", 1));
        process_batch("metrics", 2)?;
        logs.join().expect("logs worker panicked")
    })
}
