//! Streams events to TCP clients (e.g. Liminal's `tcp_input` in client mode).
//!
//! Each event is one frame: a 4-byte big-endian length followed by the event's JSON. A batch is
//! sent with a single `write_all` per client so frames arrive whole (some readers, such as
//! Liminal's `tcp_input`, are sensitive to frames split across their read timeout).

use super::{EventSink, SinkMonitor, to_json_line};
use crate::event::SimEvent;
use anyhow::Context;
use std::io::{ErrorKind, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

const ACCEPT_POLL: Duration = Duration::from_millis(50);
/// A client that doesn't read for this long is dropped rather than stalling the sink.
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);

pub struct TcpSink {
    name: String,
    local_addr: SocketAddr,
    clients: Arc<Mutex<Vec<Client>>>,
    stop: Arc<AtomicBool>,
    acceptor: Option<JoinHandle<()>>,
    monitor: SinkMonitor,
}

struct Client {
    peer: SocketAddr,
    stream: TcpStream,
}

impl TcpSink {
    /// Listens on `bind` (e.g. `127.0.0.1:9000`) and accepts any number of clients.
    pub fn bind(name: &str, bind: &str, monitor: SinkMonitor) -> anyhow::Result<Self> {
        let listener =
            TcpListener::bind(bind).with_context(|| format!("Failed to listen on {bind}"))?;
        // Non-blocking so the acceptor thread can notice `stop`.
        listener.set_nonblocking(true)?;
        let local_addr = listener.local_addr()?;
        let clients: Arc<Mutex<Vec<Client>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        monitor.set(name, status(local_addr, 0));
        tracing::info!("sink '{name}': listening on {local_addr}");

        let acceptor = {
            let (name, clients, stop, monitor) = (
                name.to_string(),
                Arc::clone(&clients),
                Arc::clone(&stop),
                monitor.clone(),
            );
            std::thread::Builder::new()
                .name(format!("sink-{name}-accept"))
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        match listener.accept() {
                            Ok((stream, peer)) => {
                                if let Err(error) = configure(&stream) {
                                    tracing::warn!("sink '{name}': could not set up {peer}: {error}");
                                    continue;
                                }
                                tracing::info!("sink '{name}': {peer} connected");
                                let mut clients = clients.lock().expect("clients lock");
                                clients.push(Client { peer, stream });
                                monitor.set(&name, status(local_addr, clients.len()));
                            }
                            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                                std::thread::sleep(ACCEPT_POLL)
                            }
                            Err(error) => {
                                tracing::warn!("sink '{name}': accept failed: {error}");
                                std::thread::sleep(ACCEPT_POLL);
                            }
                        }
                    }
                })
                .context("failed to spawn TCP acceptor thread")?
        };

        Ok(Self {
            name: name.to_string(),
            local_addr,
            clients,
            stop,
            acceptor: Some(acceptor),
            monitor,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
}

fn configure(stream: &TcpStream) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_nodelay(true)?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))
}

fn status(addr: SocketAddr, clients: usize) -> String {
    format!("listening on {addr} · {clients} connected")
}

/// Length-prefixed frames (4-byte big-endian length + JSON) for a batch of events.
pub fn encode_frames(events: &[SimEvent]) -> Vec<u8> {
    let mut frames = Vec::new();
    for event in events {
        let json = to_json_line(event);
        frames.extend_from_slice(&(json.len() as u32).to_be_bytes());
        frames.extend_from_slice(json.as_bytes());
    }
    frames
}

impl EventSink for TcpSink {
    fn write_batch(&mut self, events: &[SimEvent]) -> std::io::Result<()> {
        let mut clients = self.clients.lock().expect("clients lock");
        if clients.is_empty() || events.is_empty() {
            return Ok(());
        }
        let frames = encode_frames(events);
        let before = clients.len();
        clients.retain_mut(|client| match client.stream.write_all(&frames) {
            Ok(()) => true,
            Err(error) => {
                tracing::info!("sink '{}': {} disconnected ({error})", self.name, client.peer);
                false
            }
        });
        if clients.len() != before {
            self.monitor.set(&self.name, status(self.local_addr, clients.len()));
        }
        // A client going away is never an error for the sink itself.
        Ok(())
    }

    fn close(&mut self) -> std::io::Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
        self.clients.lock().expect("clients lock").clear();
        self.monitor.remove(&self.name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Origin;
    use crate::event::{EventBody, SCHEMA_VERSION};
    use std::io::Read;

    fn sample(seq: u64) -> SimEvent {
        SimEvent {
            v: SCHEMA_VERSION,
            seq,
            ts: 1000 + seq,
            sim_ms: seq * 20,
            body: EventBody::Action {
                entity: "mm1".into(),
                action: "pick".into(),
                command_id: seq,
                x: 4.0,
                y: 8.0,
                at: Some("machine1".into()),
                origin: Origin::Scene,
            },
        }
    }

    fn read_frame(stream: &mut TcpStream) -> SimEvent {
        let mut length = [0u8; 4];
        stream.read_exact(&mut length).unwrap();
        let mut json = vec![0u8; u32::from_be_bytes(length) as usize];
        stream.read_exact(&mut json).unwrap();
        serde_json::from_slice(&json).unwrap()
    }

    fn connect(sink: &TcpSink, monitor: &SinkMonitor, expected_clients: usize) -> TcpStream {
        let stream = TcpStream::connect(sink.local_addr()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        // Wait for the acceptor to register it.
        let want = format!("· {expected_clients} connected");
        for _ in 0..100 {
            if monitor.snapshot()["tcp"].ends_with(&want) {
                return stream;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("client not accepted: {:?}", monitor.snapshot());
    }

    #[test]
    fn sends_length_prefixed_json_frames_to_every_client() {
        let monitor = SinkMonitor::default();
        let mut sink = TcpSink::bind("tcp", "127.0.0.1:0", monitor.clone()).unwrap();
        assert!(monitor.snapshot()["tcp"].ends_with("· 0 connected"));
        // Nobody connected yet: writing is a no-op.
        sink.write_batch(&[sample(1)]).unwrap();

        let mut first = connect(&sink, &monitor, 1);
        let mut second = connect(&sink, &monitor, 2);
        let batch = vec![sample(2), sample(3)];
        sink.write_batch(&batch).unwrap();
        for client in [&mut first, &mut second] {
            assert_eq!(read_frame(client), batch[0]);
            assert_eq!(read_frame(client), batch[1]);
        }

        // A client that goes away is dropped; the other keeps receiving.
        drop(first);
        for seq in 4..20 {
            sink.write_batch(&[sample(seq)]).unwrap();
            assert_eq!(read_frame(&mut second).seq, seq);
        }
        assert!(monitor.snapshot()["tcp"].ends_with("· 1 connected"));

        sink.close().unwrap();
        assert!(monitor.snapshot().is_empty());
    }

    #[test]
    fn binding_a_busy_port_is_an_error() {
        let monitor = SinkMonitor::default();
        let sink = TcpSink::bind("a", "127.0.0.1:0", monitor.clone()).unwrap();
        let busy = sink.local_addr().to_string();
        let error = TcpSink::bind("b", &busy, monitor).err().unwrap();
        assert!(format!("{error:#}").contains(&format!("Failed to listen on {busy}")));
    }
}
