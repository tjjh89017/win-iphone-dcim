//! Byte chunks from the device thread to a reader on another thread, and a
//! one-shot value slot.
//!
//! The Explorer paste uses them: the device thread writes a file into a
//! `ChunkWriter`, and the `IStream` that Explorer reads pulls the bytes
//! from a `ChunkReader`. The channel is bounded, so the device thread
//! waits while the reader is slow. This module is portable.

use std::io::{self, Write};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The size of a full chunk.
pub const CHUNK_SIZE: usize = 256 * 1024;
/// The most chunks in the channel.
pub const CHUNK_QUEUE: usize = 8;

pub enum Chunk {
    Data(Vec<u8>),
    /// The file is complete.
    End,
    /// The read failed. No more chunks follow.
    Error(String),
}

/// A bounded chunk channel.
pub fn channel() -> (SyncSender<Chunk>, Receiver<Chunk>) {
    sync_channel(CHUNK_QUEUE)
}

/// Splits the written bytes into chunks and sends them. `on_bytes` gets
/// the size of each write, for progress.
pub struct ChunkWriter<F: FnMut(u64)> {
    tx: SyncSender<Chunk>,
    buf: Vec<u8>,
    on_bytes: F,
}

/// The error text when the reader stops early, for example when File
/// Explorer cancels a paste.
pub const CLOSED: &str = "the reader closed the stream";

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, CLOSED)
}

impl<F: FnMut(u64)> ChunkWriter<F> {
    pub fn new(tx: SyncSender<Chunk>, on_bytes: F) -> Self {
        Self {
            tx,
            buf: Vec::with_capacity(CHUNK_SIZE),
            on_bytes,
        }
    }

    fn send(&self, chunk: Chunk) -> io::Result<()> {
        self.tx.send(chunk).map_err(|_| closed())
    }

    /// Send the rest and the end mark.
    pub fn finish(mut self) -> io::Result<()> {
        self.flush()?;
        self.send(Chunk::End)
    }
}

impl<F: FnMut(u64)> Write for ChunkWriter<F> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut rest = data;
        while !rest.is_empty() {
            let n = (CHUNK_SIZE - self.buf.len()).min(rest.len());
            self.buf.extend_from_slice(&rest[..n]);
            rest = &rest[n..];
            if self.buf.len() == CHUNK_SIZE {
                let full = std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK_SIZE));
                self.send(Chunk::Data(full))?;
            }
        }
        (self.on_bytes)(data.len() as u64);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let part = std::mem::take(&mut self.buf);
        self.send(Chunk::Data(part))
    }
}

/// Send an error to the reader. The reader may be gone already.
pub fn send_error(tx: &SyncSender<Chunk>, text: String) {
    match tx.try_send(Chunk::Error(text.clone())) {
        Ok(()) | Err(TrySendError::Disconnected(_)) => {}
        // A full channel: wait until the reader takes a chunk or leaves.
        Err(TrySendError::Full(_)) => {
            let _ = tx.send(Chunk::Error(text));
        }
    }
}

/// Pulls bytes from a chunk channel.
pub struct ChunkReader {
    rx: Receiver<Chunk>,
    current: Vec<u8>,
    pos: usize,
    done: bool,
    /// An error that comes after bytes already returned.
    error: Option<String>,
}

impl ChunkReader {
    pub fn new(rx: Receiver<Chunk>) -> Self {
        Self {
            rx,
            current: Vec::new(),
            pos: 0,
            done: false,
            error: None,
        }
    }

    /// Fill `buf` as far as possible. Returns fewer bytes than `buf` holds
    /// only at the end of the file. Waits for the device thread.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let mut n = 0;
        loop {
            if self.pos < self.current.len() {
                let take = (self.current.len() - self.pos).min(buf.len() - n);
                buf[n..n + take].copy_from_slice(&self.current[self.pos..self.pos + take]);
                self.pos += take;
                n += take;
            }
            if n == buf.len() || self.done {
                return Ok(n);
            }
            if let Some(e) = self.error.take() {
                self.done = true;
                return Err(e);
            }
            match self.rx.recv() {
                Ok(Chunk::Data(data)) => {
                    self.current = data;
                    self.pos = 0;
                }
                Ok(Chunk::End) => {
                    self.done = true;
                    return Ok(n);
                }
                Ok(Chunk::Error(e)) => self.error = Some(e),
                Err(_) => self.error = Some("the device thread stopped the read".into()),
            }
            if n > 0 && self.error.is_some() {
                return Ok(n);
            }
        }
    }

    /// True after the end of the file or an error.
    pub fn is_done(&self) -> bool {
        self.done
    }
}

/// A value that one thread sets once and other threads read.
pub struct Pending<T> {
    value: Mutex<Option<T>>,
    ready: Condvar,
}

impl<T> Default for Pending<T> {
    fn default() -> Self {
        Self {
            value: Mutex::new(None),
            ready: Condvar::new(),
        }
    }
}

impl<T: Clone> Pending<T> {
    fn lock(&self) -> MutexGuard<'_, Option<T>> {
        self.value.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set(&self, value: T) {
        *self.lock() = Some(value);
        self.ready.notify_all();
    }

    /// The value if it is set. Never waits.
    pub fn get(&self) -> Option<T> {
        self.lock().clone()
    }

    /// The value, waiting at most `timeout` for it.
    pub fn wait(&self, timeout: Duration) -> Option<T> {
        let deadline = Instant::now() + timeout;
        let mut guard = self.lock();
        loop {
            if let Some(v) = guard.as_ref() {
                return Some(v.clone());
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            guard = self
                .ready
                .wait_timeout(guard, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

#[cfg(test)]
mod tests;
