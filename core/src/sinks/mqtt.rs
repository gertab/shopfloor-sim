//! Publishes events to an MQTT broker, each one to every configured topic it matches.
//!
//! The broker is external (e.g. Mosquitto) and the simulator is one of its clients. As with the TCP
//! sink, either side can start first: the sink keeps reconnecting, and events produced while the
//! broker is unreachable are dropped rather than queued, so subscribers only see what happens while
//! it is connected. Events from before the first connection attempt has finished are queued, so a
//! run started with the broker up delivers everything from `sim started`.

use super::{EventSink, SinkMonitor, to_json_line};
use crate::event::SimEvent;
use crate::scenario::{MqttTopic, parse_broker};
use anyhow::{anyhow, bail};
use rumqttc::{Client, Connection, Event, MqttOptions, Outgoing, Packet, QoS};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Publishes the client can queue for its event loop. Beyond that, new ones are dropped.
const QUEUE_CAPACITY: usize = 4096;
/// Wait between connection attempts while the broker can't be reached.
const RETRY: Duration = Duration::from_secs(1);
const STOP_POLL: Duration = Duration::from_millis(20);
/// How long `close` waits for queued events to go out before leaving the event loop to finish.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

pub struct MqttSink {
    name: String,
    topics: Vec<MqttTopic>,
    qos: QoS,
    client: Option<Client>,
    shared: Arc<Shared>,
    event_loop: Option<JoinHandle<()>>,
    monitor: SinkMonitor,
    /// Publishes are being dropped because the queue is full (warned about once per episode).
    queue_full: bool,
}

struct Shared {
    connected: AtomicBool,
    /// Publishes are accepted: connected, or the first attempt is still under way.
    open: AtomicBool,
    stop: AtomicBool,
}

impl Shared {
    fn new() -> Self {
        Self {
            connected: AtomicBool::new(false),
            open: AtomicBool::new(true),
            stop: AtomicBool::new(false),
        }
    }
}

impl MqttSink {
    /// Starts connecting to `broker` in the background. A broker that can't be reached is not an
    /// error: the sink keeps trying, and its status says so.
    pub fn connect(
        name: &str,
        broker: &str,
        topics: &[MqttTopic],
        qos: u8,
        client_id: Option<&str>,
        monitor: SinkMonitor,
    ) -> anyhow::Result<Self> {
        let (host, port) = parse_broker(broker).map_err(|error| anyhow!(error))?;
        let qos = match qos {
            0 => QoS::AtMostOnce,
            1 => QoS::AtLeastOnce,
            2 => QoS::ExactlyOnce,
            other => bail!("qos must be 0, 1 or 2 (got {other})"),
        };
        // The process id keeps the app and a CLI run apart on the same broker, which would
        // otherwise disconnect one of them for reusing a client id.
        let client_id = client_id
            .map(str::to_string)
            .unwrap_or_else(|| format!("shopfloor-{name}-{}", std::process::id()));
        let address = format!("{host}:{port}");
        let options = MqttOptions::new(client_id, host, port);
        let (client, connection) = Client::new(options, QUEUE_CAPACITY);
        let shared = Arc::new(Shared::new());
        monitor.set(name, connecting(&address));

        let event_loop = {
            let (name, shared, monitor) = (name.to_string(), Arc::clone(&shared), monitor.clone());
            std::thread::Builder::new()
                .name(format!("sink-{name}-mqtt"))
                .spawn(move || run_event_loop(&name, &address, connection, &shared, &monitor))?
        };

        Ok(Self {
            name: name.to_string(),
            topics: topics.to_vec(),
            qos,
            client: Some(client),
            shared,
            event_loop: Some(event_loop),
            monitor,
            queue_full: false,
        })
    }
}

/// The topics an event is published to, in the order they are configured.
fn topics_for<'a>(topics: &'a [MqttTopic], event: &SimEvent) -> Vec<&'a str> {
    topics
        .iter()
        .filter(|topic| topic.matches(event))
        .map(|topic| topic.topic.as_str())
        .collect()
}

fn connecting(address: &str) -> String {
    format!("connecting to {address}")
}

/// Drives the connection: reconnects while the broker is away, and ends once the sink has
/// disconnected or been dropped.
fn run_event_loop(
    name: &str,
    address: &str,
    mut connection: Connection,
    shared: &Shared,
    monitor: &SinkMonitor,
) {
    // So a broker that stays down is logged once, not on every attempt.
    let mut outage_logged = false;
    loop {
        match connection.recv() {
            // Every client handle is gone, and what they queued has been sent.
            Err(_) => break,
            Ok(Ok(Event::Incoming(Packet::ConnAck(_)))) => {
                shared.connected.store(true, Ordering::Relaxed);
                shared.open.store(true, Ordering::Relaxed);
                outage_logged = false;
                monitor.set(name, format!("connected to {address}"));
                tracing::info!("sink '{name}': connected to MQTT broker {address}");
            }
            Ok(Ok(Event::Outgoing(Outgoing::Disconnect))) => break,
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                let was_connected = shared.connected.swap(false, Ordering::Relaxed);
                shared.open.store(false, Ordering::Relaxed);
                if shared.stop.load(Ordering::Relaxed) {
                    break;
                }
                monitor.set(name, connecting(address));
                if was_connected {
                    tracing::warn!("sink '{name}': lost broker {address} ({error}), reconnecting");
                } else if !outage_logged {
                    tracing::warn!(
                        "sink '{name}': cannot reach broker {address} ({error}), retrying every {} s",
                        RETRY.as_secs()
                    );
                }
                outage_logged = true;
                let retry_at = Instant::now() + RETRY;
                while Instant::now() < retry_at && !shared.stop.load(Ordering::Relaxed) {
                    std::thread::sleep(STOP_POLL);
                }
                if shared.stop.load(Ordering::Relaxed) {
                    break;
                }
            }
        }
    }
    shared.connected.store(false, Ordering::Relaxed);
    shared.open.store(false, Ordering::Relaxed);
}

impl EventSink for MqttSink {
    fn write_batch(&mut self, events: &[SimEvent]) -> std::io::Result<()> {
        let Some(client) = &self.client else {
            return Ok(());
        };
        if !self.shared.open.load(Ordering::Relaxed) {
            return Ok(());
        }
        for event in events {
            let topics = topics_for(&self.topics, event);
            if topics.is_empty() {
                continue;
            }
            let payload = to_json_line(event);
            for topic in topics {
                match client.try_publish(topic, self.qos, false, payload.clone()) {
                    Ok(()) => self.queue_full = false,
                    Err(error) => {
                        if !self.queue_full {
                            tracing::warn!("sink '{}': dropping events ({error})", self.name);
                        }
                        self.queue_full = true;
                    }
                }
            }
        }
        // Broker trouble is never an error for the sink itself.
        Ok(())
    }

    fn close(&mut self) -> std::io::Result<()> {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(client) = self.client.take() {
            // Goes out after whatever is still queued; then the event loop ends.
            let _ = client.try_disconnect();
        }
        if let Some(event_loop) = self.event_loop.take() {
            let deadline = Instant::now() + CLOSE_TIMEOUT;
            while !event_loop.is_finished() && Instant::now() < deadline {
                std::thread::sleep(STOP_POLL);
            }
            if event_loop.is_finished() {
                let _ = event_loop.join();
            } else {
                tracing::warn!("sink '{}': still sending to the broker, not waiting", self.name);
            }
        }
        self.monitor.remove(&self.name);
        Ok(())
    }
}

impl Drop for MqttSink {
    fn drop(&mut self) {
        // Without this, an event loop whose broker is down would keep retrying forever.
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Origin;
    use crate::event::{EventBody, Lifecycle, SCHEMA_VERSION};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    fn event(seq: u64, body: EventBody) -> SimEvent {
        SimEvent {
            v: SCHEMA_VERSION,
            seq,
            timestamp_ms: 1000 + seq,
            sim_ms: seq * 20,
            body,
        }
    }

    fn sensor_reading(seq: u64) -> SimEvent {
        event(
            seq,
            EventBody::Sensor {
                sensor: "mm1.vibration".into(),
                device_id: "mm1".into(),
                sensor_type: "vibration".into(),
                value: 1.2,
                unit: "g".into(),
                x: 4.0,
                y: 8.0,
            },
        )
    }

    fn pick(seq: u64, device_id: &str) -> SimEvent {
        event(
            seq,
            EventBody::Action {
                device_id: device_id.into(),
                event: "pick".into(),
                object: None,
                command_id: seq,
                x: 4.0,
                y: 8.0,
                at: None,
                origin: Origin::Scene,
            },
        )
    }

    fn sim_started(seq: u64) -> SimEvent {
        event(
            seq,
            EventBody::Sim {
                state: Lifecycle::Started,
                scenario: "Test".into(),
            },
        )
    }

    fn topic(name: &str, kinds: &[&str], entities: &[&str]) -> MqttTopic {
        MqttTopic {
            topic: name.into(),
            kinds: kinds.iter().map(|kind| kind.to_string()).collect(),
            entities: entities.iter().map(|entity| entity.to_string()).collect(),
        }
    }

    fn sample_topics() -> Vec<MqttTopic> {
        vec![
            topic("shopfloor/sensors", &["sensor", "sensor_set"], &[]),
            topic("shopfloor/mm1", &[], &["mm1"]),
            topic("shopfloor/all", &[], &[]),
        ]
    }

    #[test]
    fn events_go_to_every_topic_they_match() {
        let topics = sample_topics();
        assert_eq!(
            topics_for(&topics, &sensor_reading(1)),
            ["shopfloor/sensors", "shopfloor/mm1", "shopfloor/all"]
        );
        assert_eq!(topics_for(&topics, &pick(2, "mm1")), ["shopfloor/mm1", "shopfloor/all"]);
        assert_eq!(topics_for(&topics, &pick(3, "mm2")), ["shopfloor/all"]);
        // No entity: only topics without an entity filter.
        assert_eq!(topics_for(&topics, &sim_started(4)), ["shopfloor/all"]);
        // Both filters must match.
        let both = [topic("t", &["action"], &["mm2"])];
        assert_eq!(topics_for(&both, &pick(5, "mm2")), ["t"]);
        assert!(topics_for(&both, &pick(6, "mm1")).is_empty());
        assert!(topics_for(&both, &sensor_reading(7)).is_empty());
    }

    /// Reads one MQTT packet: its first header byte and its body.
    fn read_packet(stream: &mut TcpStream) -> (u8, Vec<u8>) {
        let mut header = [0u8; 1];
        stream.read_exact(&mut header).unwrap();
        let (mut length, mut shift) = (0usize, 0);
        loop {
            let mut byte = [0u8; 1];
            stream.read_exact(&mut byte).unwrap();
            length |= usize::from(byte[0] & 0x7f) << shift;
            if byte[0] & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        let mut body = vec![0u8; length];
        stream.read_exact(&mut body).unwrap();
        (header[0], body)
    }

    /// Plays a broker for one client: takes its CONNECT and accepts it.
    fn accept_client(listener: &TcpListener) -> TcpStream {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let (header, _) = read_packet(&mut stream);
        assert_eq!(header >> 4, 1, "CONNECT");
        stream.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap(); // CONNACK, accepted
        stream
    }

    /// Topic and payload of a QoS 0 PUBLISH.
    fn read_publish(stream: &mut TcpStream) -> (String, String) {
        let (header, body) = read_packet(stream);
        assert_eq!(header, 0x30, "QoS 0 PUBLISH, not retained");
        let length = usize::from(u16::from_be_bytes([body[0], body[1]]));
        let topic = String::from_utf8(body[2..2 + length].to_vec()).unwrap();
        let payload = String::from_utf8(body[2 + length..].to_vec()).unwrap();
        (topic, payload)
    }

    fn wait_for_status(monitor: &SinkMonitor, prefix: &str) {
        for _ in 0..300 {
            if monitor.snapshot().get("mqtt").is_some_and(|status| status.starts_with(prefix)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("status never started with {prefix:?}: {:?}", monitor.snapshot());
    }

    #[test]
    fn publishes_each_event_to_its_topics_and_reconnects() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let broker = listener.local_addr().unwrap().to_string();
        let monitor = SinkMonitor::default();
        let mut sink =
            MqttSink::connect("mqtt", &broker, &sample_topics(), 0, None, monitor.clone()).unwrap();

        // Written before the broker has accepted the connection: queued until it does.
        let batch = [sensor_reading(1), pick(2, "mm1"), sim_started(3)];
        sink.write_batch(&batch).unwrap();
        let mut stream = accept_client(&listener);
        wait_for_status(&monitor, &format!("connected to {broker}"));
        let expected = [
            ("shopfloor/sensors", &batch[0]),
            ("shopfloor/mm1", &batch[0]),
            ("shopfloor/all", &batch[0]),
            ("shopfloor/mm1", &batch[1]),
            ("shopfloor/all", &batch[1]),
            ("shopfloor/all", &batch[2]),
        ];
        for (topic, event) in expected {
            assert_eq!(read_publish(&mut stream), (topic.to_string(), to_json_line(event)));
        }

        // The broker goes away and comes back: the sink reconnects by itself, and what happened
        // in between is dropped.
        drop(stream);
        wait_for_status(&monitor, "connecting to");
        sink.write_batch(&[sim_started(4)]).unwrap();
        let mut stream = accept_client(&listener);
        wait_for_status(&monitor, "connected to");
        sink.write_batch(&[sim_started(5)]).unwrap();
        let expected = ("shopfloor/all".to_string(), to_json_line(&sim_started(5)));
        assert_eq!(read_publish(&mut stream), expected);

        sink.close().unwrap();
        assert_eq!(read_packet(&mut stream).0, 0xe0, "DISCONNECT");
        assert!(monitor.snapshot().is_empty());
    }

    #[test]
    fn a_missing_broker_is_not_an_error() {
        // A port nobody listens on.
        let broker = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
        let monitor = SinkMonitor::default();
        let mut sink =
            MqttSink::connect("mqtt", &broker, &sample_topics(), 0, None, monitor.clone()).unwrap();
        sink.write_batch(&[sensor_reading(1)]).unwrap();
        assert_eq!(monitor.snapshot()["mqtt"], format!("connecting to {broker}"));

        let started = Instant::now();
        sink.close().unwrap();
        assert!(started.elapsed() < CLOSE_TIMEOUT, "close took {:?}", started.elapsed());
        assert!(monitor.snapshot().is_empty());
    }
}
