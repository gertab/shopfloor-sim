use super::*;
use crate::command::{Origin, SimCommand, Target};
use crate::scenario::parse_scenario;
use std::path::Path;

fn scenario_recording_to(path: &Path) -> Arc<Scenario> {
    let text = format!(
        r#"
        name = "Runtime test"

        [floor]
        width_m = 10.0
        height_m = 5.0

        [machines.m1]
        position = [8.0, 4.0]
        size = [1.0, 1.0]        # dock (8.0, 3.0)

        [mobile_manipulators.mm1]
        start = [2.0, 3.0]
        max_speed_mps = 2.0

        [mobile_manipulators.mm1.sensors.vibration]
        default = 0.1
        range = [0.0, 1.0]
        period_ms = 100

        [actions.undock]

        [output.sinks.rec]
        type = "jsonl"
        path = '{}'
        append = false
        "#,
        path.display()
    );
    Arc::new(parse_scenario(&text, None).unwrap())
}

fn api(command: SimCommand) -> CommandEnvelope {
    CommandEnvelope::new(Origin::Api, command)
}

#[tokio::test(start_paused = true)]
async fn drives_engine_in_real_time_and_records_to_jsonl() {
    let path = std::env::temp_dir().join(format!("shopfloor-runtime-test-{}.jsonl", std::process::id()));
    let runtime = SimRuntime::spawn(Engine::new(scenario_recording_to(&path))).unwrap();
    let mut events = runtime.subscribe();

    runtime
        .send(api(SimCommand::MoveTo {
            mm: "mm1".into(),
            target: Target::Machine("m1".into()),
        }))
        .await
        .unwrap();
    let error = runtime
        .send(api(SimCommand::Stop { mm: "mm9".into() }))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Unknown mobile manipulator"));

    // 6 m at 2 m/s takes 3 s of sim time.
    tokio::time::sleep(Duration::from_secs(4)).await;
    let mut kinds = Vec::new();
    while let Ok(batch) = events.try_recv() {
        kinds.extend(batch.iter().map(SimEvent::kind));
    }
    assert!(kinds.contains(&"position"));
    assert!(kinds.contains(&"sensor"));
    assert_eq!(kinds.iter().filter(|k| **k == "arrived").count(), 1);
    let state = runtime.state();
    assert_eq!(state.mobile_manipulators[0].at.as_deref(), Some("m1"));
    assert!(
        state.sim_ms >= 3000,
        "sim time follows the clock: {}",
        state.sim_ms
    );

    runtime.control(Control::Pause).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let paused_at = runtime.state().sim_ms;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(runtime.state().paused);
    assert_eq!(runtime.state().sim_ms, paused_at);

    runtime.shutdown().await;

    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).ok();
    let recorded: Vec<SimEvent> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(recorded.first().unwrap().kind(), "sim");
    assert_eq!(recorded.last().unwrap().kind(), "sim"); // paused
    let seqs: Vec<u64> = recorded.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, (1..=recorded.len() as u64).collect::<Vec<_>>());
    assert!(recorded.iter().any(|e| e.kind() == "arrived"));
}

#[tokio::test(start_paused = true)]
async fn replays_a_trace_in_real_time_and_records_it_as_written() {
    use crate::replay::{Replay, ReplayOptions};
    use crate::trace::parse_trace;

    let path = std::env::temp_dir().join(format!("shopfloor-replay-test-{}.jsonl", std::process::id()));
    let trace = parse_trace(
        "t.json",
        r#"{"version": 1, "steps": [
          {"type": "send", "payload": {"device_id": "mm1", "event": "undock", "target": "m1"}},
          {"type": "delay", "duration_ms": 500},
          {"type": "send", "payload": {"type": "vibration", "device_id": "mm1", "value": 0.20}}
        ]}"#,
    )
    .unwrap();
    let scenario = scenario_recording_to(&path);
    let (replay, _) = Replay::new(scenario, vec![trace], ReplayOptions::default(), 0).unwrap();
    let runtime = ReplayRuntime::spawn(replay).unwrap();
    let error = runtime.control(Control::PlayScene("x".into())).await.unwrap_err();
    assert!(error.to_string().contains("no scenes"));

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !runtime.state().replay.unwrap().done,
        "the second payload is 500 ms in"
    );
    tokio::time::sleep(Duration::from_secs(1)).await;
    let status = runtime.state().replay.unwrap();
    assert_eq!((status.sent, status.done), (2, true));
    runtime.shutdown().await;

    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).ok();
    assert_eq!(
        text,
        "{\"device_id\":\"mm1\",\"event\":\"undock\",\"target\":\"m1\"}\n\
         {\"type\":\"vibration\",\"device_id\":\"mm1\",\"value\":0.20}\n"
    );
}

#[tokio::test(start_paused = true)]
async fn a_scenario_can_open_paused() {
    let path = std::env::temp_dir().join(format!(
        "shopfloor-start-paused-test-{}.jsonl",
        std::process::id()
    ));
    let mut scenario = (*scenario_recording_to(&path)).clone();
    scenario.sim.start_paused = true;
    let runtime = SimRuntime::spawn(Engine::new(Arc::new(scenario))).unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let state = runtime.state();
    assert!(state.paused);
    assert_eq!(state.sim_ms, 0, "the clock waits");

    runtime.control(Control::Resume).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(runtime.state().sim_ms > 0);
    runtime.shutdown().await;

    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).ok();
    let lifecycle: Vec<String> = text
        .lines()
        .map(|line| serde_json::from_str::<SimEvent>(line).unwrap())
        .filter_map(|event| match event.body {
            crate::event::EventBody::Sim { state, .. } => Some(format!("{state:?}")),
            _ => None,
        })
        .collect();
    assert_eq!(lifecycle, ["Started", "Paused", "Resumed"]);
}

/// One length-prefixed frame from a TCP sink, as text.
fn read_frame(stream: &mut std::net::TcpStream) -> String {
    use std::io::Read;
    let mut length = [0u8; 4];
    stream.read_exact(&mut length).unwrap();
    let mut json = vec![0u8; u32::from_be_bytes(length) as usize];
    stream.read_exact(&mut json).unwrap();
    String::from_utf8(json).unwrap()
}

/// Shared outputs outlive the runtimes writing to them: a TCP client stays connected while a
/// simulation is replaced by a replay, as when the app switches between them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_outputs_stay_open_across_runtimes() {
    use crate::replay::{Replay, ReplayOptions};
    use crate::trace::parse_trace;
    use std::io::Read;

    let scenario = Arc::new(
        parse_scenario(
            r#"
            name = "Outputs test"

            [floor]
            width_m = 10.0
            height_m = 5.0

            [mobile_manipulators.mm1]
            start = [2.0, 3.0]

            [actions.pick]

            [output.sinks.tcp]
            type = "tcp"
            bind = "127.0.0.1:0"
            "#,
            None,
        )
        .unwrap(),
    );
    let outputs = Outputs::open(&scenario).unwrap();
    let connected = |clients: usize| outputs.status()["tcp"].ends_with(&format!("· {clients} connected"));
    // "listening on 127.0.0.1:PORT · 0 connected"
    let address = outputs.status()["tcp"]
        .trim_start_matches("listening on ")
        .split(' ')
        .next()
        .unwrap()
        .to_string();
    let mut client = std::net::TcpStream::connect(&address).unwrap();
    client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    for _ in 0..200 {
        if connected(1) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(connected(1));

    // A simulation (an idle MM and no sensors: it only says it started), then shut down.
    let simulation = SimRuntime::spawn_with(Engine::new(Arc::clone(&scenario)), outputs.writer());
    let started = read_frame(&mut client);
    assert!(
        started.contains(r#""kind":"sim""#) && started.contains(r#""state":"started""#),
        "{started}"
    );
    simulation.shutdown().await;
    assert!(connected(1), "the simulation's end leaves the client connected");

    // Then a replay, on the same connection.
    let trace = parse_trace(
        "t.json",
        r#"{"version": 1, "steps": [{"type": "send", "payload": {"device_id": "mm1", "event": "pick"}}]}"#,
    )
    .unwrap();
    let (replay, _) = Replay::new(Arc::clone(&scenario), vec![trace], ReplayOptions::default(), 0).unwrap();
    let replay = ReplayRuntime::spawn_with(replay, outputs.writer());
    assert_eq!(read_frame(&mut client), r#"{"device_id":"mm1","event":"pick"}"#);
    replay.shutdown().await;

    // Closing the outputs ends the connection.
    outputs.close().await;
    let mut byte = [0u8; 1];
    assert_eq!(client.read(&mut byte).unwrap(), 0, "connection closed");
}

#[tokio::test]
async fn spawn_fails_when_a_sink_cannot_open() {
    let scenario = scenario_recording_to(Path::new("/dev/null/not-a-dir/x.jsonl"));
    let error = SimRuntime::spawn(Engine::new(scenario)).err().unwrap();
    assert!(format!("{error:#}").contains("Failed to"));
}

/// Outputs that fail to open close the sinks they had opened before returning, so what those hold
/// (a port) is free at once: the app then opens the previous scenario's outputs again.
#[tokio::test]
async fn outputs_that_fail_to_open_free_what_they_opened() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    // The TCP sink opens first (sinks open in name order), then the JSONL one fails.
    let scenario = parse_scenario(
        &format!(
            r#"
            [floor]
            width_m = 10.0
            height_m = 5.0

            [output.sinks.a_tcp]
            type = "tcp"
            bind = "127.0.0.1:{port}"

            [output.sinks.b_rec]
            type = "jsonl"
            path = "/dev/null/not-a-dir/x.jsonl"
            "#
        ),
        None,
    )
    .unwrap();
    assert!(Outputs::open(&scenario).is_err());
    std::net::TcpListener::bind(("127.0.0.1", port)).expect("the port is free again");
}
