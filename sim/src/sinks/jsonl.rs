use super::{EventSink, to_json_line};
use crate::event::SimEvent;
use anyhow::Context;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;

/// Writes one JSON line per event to a file, flushing after every batch.
pub struct JsonlSink {
    writer: BufWriter<File>,
}

impl JsonlSink {
    pub fn open(path: &Path, append: bool) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(path)
            .with_context(|| format!("Failed to open {}", path.display()))?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }
}

impl EventSink for JsonlSink {
    fn write_batch(&mut self, events: &[SimEvent]) -> std::io::Result<()> {
        for event in events {
            writeln!(self.writer, "{}", to_json_line(event))?;
        }
        self.writer.flush()
    }

    fn close(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}
