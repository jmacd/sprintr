// Copyright The OpenTelemetry Authors
// SPDX-License-Identifier: Apache-2.0
// Adapted from the original buffer implementation; see NOTICE for provenance.

//! Reusable heap or fixed stack storage for complete JSON records.
//!
//! Storage is independent of encoding. Capacity failures roll back the record
//! rather than truncating JSON or dropping a positional argument.

use std::io::{self, Write};

/// Byte storage supporting record-level rollback.
pub trait RecordBuffer: Write {
    /// Currently encoded bytes.
    fn as_slice(&self) -> &[u8];

    /// Reset the write position while retaining storage.
    fn clear(&mut self);

    /// Roll back to a previous byte position.
    fn truncate(&mut self, length: usize);

    /// Either complete an encoding or restore the original write position.
    fn try_atomic<T>(&mut self, encode: impl FnOnce(&mut Self) -> io::Result<T>) -> io::Result<T>
    where
        Self: Sized,
    {
        let checkpoint = self.as_slice().len();
        match encode(self) {
            Ok(value) => Ok(value),
            Err(error) => {
                self.truncate(checkpoint);
                Err(error)
            }
        }
    }
}

impl<B: RecordBuffer + ?Sized> RecordBuffer for &mut B {
    fn as_slice(&self) -> &[u8] {
        (**self).as_slice()
    }

    fn clear(&mut self) {
        (**self).clear();
    }

    fn truncate(&mut self, length: usize) {
        (**self).truncate(length);
    }
}

/// Heap storage reused between records, optionally limited in encoded bytes.
pub struct HeapBuffer {
    bytes: Vec<u8>,
    limit: usize,
}

impl HeapBuffer {
    /// Preallocate storage, permitting growth for larger records.
    pub fn with_capacity(capacity: usize) -> Self {
        Self::with_capacity_and_limit(capacity, usize::MAX)
    }

    /// Limit encoded record size, including JSON escaping and the newline.
    pub fn with_capacity_and_limit(capacity: usize, limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity.min(limit)),
            limit,
        }
    }
}

impl Default for HeapBuffer {
    fn default() -> Self {
        Self::with_capacity(256)
    }
}

impl RecordBuffer for HeapBuffer {
    fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    fn clear(&mut self) {
        self.bytes.clear();
    }

    fn truncate(&mut self, length: usize) {
        self.bytes.truncate(length);
    }
}

impl Write for HeapBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::ErrorKind::StorageFull.into());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Fixed-capacity storage with no heap allocation, even on the first record.
pub struct StackBuffer<const N: usize> {
    bytes: [u8; N],
    length: usize,
}

impl<const N: usize> StackBuffer<N> {
    /// Create empty stack storage.
    pub const fn new() -> Self {
        Self {
            bytes: [0; N],
            length: 0,
        }
    }
}

impl<const N: usize> Default for StackBuffer<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> RecordBuffer for StackBuffer<N> {
    fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.length]
    }

    fn clear(&mut self) {
        self.length = 0;
    }

    fn truncate(&mut self, length: usize) {
        self.length = self.length.min(length);
    }
}

impl<const N: usize> Write for StackBuffer<N> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > N - self.length {
            return Err(io::ErrorKind::StorageFull.into());
        }
        self.bytes[self.length..self.length + bytes.len()].copy_from_slice(bytes);
        self.length += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
