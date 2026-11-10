use super::{EventSink, to_json_line};
use crate::event::SimEvent;
use std::io::Write;

/// Writes one JSON line per event to stdout.
pub struct StdoutSink;

impl EventSink for StdoutSink {
    fn write_batch(&mut self, events: &[SimEvent]) -> std::io::Result<()> {
        let mut out = std::io::stdout().lock();
        for event in events {
            writeln!(out, "{}", to_json_line(event))?;
        }
        out.flush()
    }
}
