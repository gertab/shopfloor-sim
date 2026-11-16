use super::{EventSink, Message};
use std::io::Write;

/// Writes one JSON line per event to stdout.
pub struct StdoutSink;

impl EventSink for StdoutSink {
    fn write_batch(&mut self, messages: &[Message]) -> std::io::Result<()> {
        let mut out = std::io::stdout().lock();
        for message in messages {
            writeln!(out, "{}", message.json)?;
        }
        out.flush()
    }
}
