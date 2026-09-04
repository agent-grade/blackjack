//! Why the wrapper did what it did, for a user who watched it do something
//! else. Every decision is stamped with the milliseconds since the session
//! began, so the order of two of them on two threads is readable afterwards.
//!
//! The bytes the wrapped program draws are never written here, only what was
//! made of them — with one exception, at the end: the final grid, the screen
//! model rendered whole, which is a screenful of unstamped lines of whatever
//! the program last drew.

use std::fmt::Arguments;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const LOG_VAR: &str = "BLACKJACK_DEBUG_LOG";

/// One handle behind one lock, because the threads that write here run
/// independently: separate appenders interleave inside a line, and the grid
/// dump is far too long to rely on a write landing whole.
#[derive(Clone)]
pub struct Log(Option<Arc<Sink>>);

struct Sink {
    file: Mutex<File>,
    started: Instant,
}

impl Log {
    pub fn open() -> Self {
        Self(
            std::env::var_os(LOG_VAR)
                .and_then(|path| OpenOptions::new().create(true).append(true).open(path).ok())
                .map(|file| {
                    Arc::new(Sink {
                        file: Mutex::new(file),
                        started: Instant::now(),
                    })
                }),
        )
    }

    /// A log nothing is written to. Every session that did not ask for one has
    /// this, and so does every test that is not about the log.
    #[cfg(test)]
    pub fn off() -> Self {
        Self(None)
    }

    /// A log written where a test can read back what a session decided.
    #[cfg(test)]
    pub fn writing(path: &std::path::Path) -> Self {
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .expect("a log to write");
        Self(Some(Arc::new(Sink {
            file: Mutex::new(file),
            started: Instant::now(),
        })))
    }

    /// Worth asking before building a line: the callers on the keystroke path
    /// allocate to say anything at all.
    pub fn is_on(&self) -> bool {
        self.0.is_some()
    }

    pub fn line(&self, line: Arguments) {
        let Some(sink) = &self.0 else {
            return;
        };
        if let Ok(mut file) = sink.file.lock() {
            let _ = writeln!(file, "{:>7} {line}", sink.started.elapsed().as_millis());
        }
    }
}

/// Bytes as a log line can carry them: an escape byte has to survive legibly
/// while typed text stays readable.
pub fn escape(bytes: &[u8]) -> String {
    let mut out = Vec::with_capacity(bytes.len());
    for &byte in bytes {
        match byte {
            b'"' | b'\\' => out.extend_from_slice(&[b'\\', byte]),
            0x20..=0x7e | 0x80.. => out.push(byte),
            _ => out.extend_from_slice(format!("\\x{byte:02x}").as_bytes()),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
