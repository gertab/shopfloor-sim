use super::*;
use crate::scenario::parse_scenario;
use crate::trace::parse_trace;
use std::sync::Mutex;

const SCENARIO: &str = r#"
    name = "Replay test"

    [floor]
    width_m = 12.0
    height_m = 8.0

    [machines.stocker]
    position = [6.0, 6.0]
    size = [2.0, 1.0]          # dock (6.0, 5.0)

    [machines.bonder]
    position = [2.0, 6.0]
    size = [2.0, 1.0]          # dock (2.0, 5.0)

    [mobile_manipulators.mm-1]
    start_at = "stocker"
    max_speed_mps = 1.0

    [mobile_manipulators.mm-1.sensors.vibration]
    unit = "m/s^2"
    default = 0.0
    range = [0.0, 5.0]

    [mobile_manipulators.mm-1.sensors.lidar]
    unit = "m"
    default = 0.0
    range = [0.0, 10.0]

    [mobile_manipulators.mm-2]
    start = [10.0, 2.0]

    [actions.pick]
    requires_machine = true

    [actions.place]
    requires_machine = true

    [actions.dock]

    [actions.undock]
"#;

/// mm-1 picks at the stocker, carries the dies to the bonder and places them there.
const EVENTS: &str = r#"{
  "version": 1,
  "steps": [
    {"type": "send", "payload": {"device_id": "mm-1", "event": "pick", "object": "dies", "timestamp_ms": 1000000}},
    {"type": "delay", "duration_ms": 1000},
    {"type": "send", "payload": {"device_id": "mm-1", "event": "undock", "target": "stocker", "timestamp_ms": 1001000}},
    {"type": "delay", "duration_ms": 1000},
    {"type": "send", "payload": {"device_id": "mm-1", "event": "move", "source": "stocker", "destination": "bonder", "timestamp_ms": 1002000}},
    {"type": "delay", "duration_ms": 2000},
    {"type": "send", "payload": {"device_id": "mm-1", "event": "dock", "target": "bonder", "timestamp_ms": 1004000}},
    {"type": "delay", "duration_ms": 1000},
    {"type": "send", "payload": {"device_id": "mm-1", "event": "place", "object": "dies", "timestamp_ms": 1005000}}
  ]
}"#;

const SENSORS: &str = r#"{
  "version": 1,
  "steps": [
    {"type": "send", "payload": {
        "type": "vibration", "device_id": "mm-1", "value": 1.250, "unit": "m/s^2", "timestamp_ms": 1000000
    }},
    {"type": "delay", "duration_ms": 2500},
    {"type": "send", "payload": {"type": "lidar", "device_id": "mm-1", "value": 0.5, "unit": "m", "target": "bonder", "timestamp_ms": 1002500}}
  ]
}"#;

const START_TS: u64 = 1_761_730_000_000;

fn scenario() -> Arc<Scenario> {
    Arc::new(parse_scenario(SCENARIO, None).unwrap())
}

fn traces(texts: &[(&str, &str)]) -> Vec<Trace> {
    texts
        .iter()
        .map(|(name, text)| parse_trace(name, text).unwrap())
        .collect()
}

fn replay(texts: &[(&str, &str)], options: ReplayOptions) -> (Replay, Vec<Finding>) {
    Replay::new(scenario(), traces(texts), options, START_TS).unwrap()
}

fn sample_replay(options: ReplayOptions) -> Replay {
    let (replay, findings) = replay(&[("events.json", EVENTS), ("sensors.json", SENSORS)], options);
    assert_eq!(findings, vec![], "the sample traces fit the scenario");
    replay
}

struct MemorySink(Arc<Mutex<Vec<Message>>>);

impl EventSink for MemorySink {
    fn write_batch(&mut self, messages: &[Message]) -> std::io::Result<()> {
        self.0.lock().unwrap().extend_from_slice(messages);
        Ok(())
    }
}

fn play(replay: &mut Replay) -> Vec<Message> {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let mut sinks: Vec<Box<dyn EventSink>> = vec![Box::new(MemorySink(Arc::clone(&messages)))];
    let written = run_fast(replay, &mut sinks).unwrap();
    let messages = messages.lock().unwrap().clone();
    assert_eq!(written as usize, messages.len());
    messages
}

#[test]
fn the_payload_format_sends_exactly_the_payloads_at_their_times() {
    let mut replay = sample_replay(ReplayOptions::default());
    let messages = play(&mut replay);
    let sent: Vec<(u64, &str)> = messages.iter().map(|m| (m.sim_ms, m.json.as_str())).collect();
    assert_eq!(
        sent,
        vec![
            (
                0,
                r#"{"device_id":"mm-1","event":"pick","object":"dies","timestamp_ms":1000000}"#
            ),
            (
                0,
                r#"{"type":"vibration","device_id":"mm-1","value":1.250,"unit":"m/s^2","timestamp_ms":1000000}"#
            ),
            (
                1000,
                r#"{"device_id":"mm-1","event":"undock","target":"stocker","timestamp_ms":1001000}"#
            ),
            (
                2000,
                r#"{"device_id":"mm-1","event":"move","source":"stocker","destination":"bonder","timestamp_ms":1002000}"#
            ),
            (
                2500,
                r#"{"type":"lidar","device_id":"mm-1","value":0.5,"unit":"m","target":"bonder","timestamp_ms":1002500}"#
            ),
            (
                4000,
                r#"{"device_id":"mm-1","event":"dock","target":"bonder","timestamp_ms":1004000}"#
            ),
            (
                5000,
                r#"{"device_id":"mm-1","event":"place","object":"dies","timestamp_ms":1005000}"#
            ),
        ]
    );
    let routing: Vec<(Option<&str>, Option<&str>)> = messages
        .iter()
        .map(|m| (m.kind, m.device_id.as_deref()))
        .collect();
    assert_eq!(
        routing,
        vec![
            (Some("action"), Some("mm-1")),
            (Some("sensor"), Some("mm-1")),
            (Some("action"), Some("mm-1")), // undock: an action, like pick
            (Some("move_command"), Some("mm-1")),
            (Some("sensor"), Some("mm-1")),
            (Some("action"), Some("mm-1")), // dock
            (Some("action"), Some("mm-1")),
        ]
    );
    assert_eq!(
        messages.iter().map(|m| m.seq).collect::<Vec<_>>(),
        (1..=7).collect::<Vec<_>>()
    );
    assert!(replay.is_done());
    assert_eq!(replay.state().sim_ms, 5000, "time stops at the end");
}

#[test]
fn the_envelope_format_adds_the_simulator_fields_in_front() {
    let mut replay = sample_replay(ReplayOptions {
        format: Format::Envelope,
        positions: false,
    });
    let messages = play(&mut replay);
    assert_eq!(
        messages[1].json,
        r#"{"v":1,"seq":2,"sim_ms":0,"kind":"sensor","type":"vibration","device_id":"mm-1","value":1.250,"unit":"m/s^2","timestamp_ms":1000000}"#
    );
    assert_eq!(
        messages[3].json,
        r#"{"v":1,"seq":4,"sim_ms":2000,"kind":"move_command","device_id":"mm-1","event":"move","source":"stocker","destination":"bonder","timestamp_ms":1002000}"#
    );
}

#[test]
fn the_envelope_adds_a_timestamp_only_when_the_payload_has_none() {
    assert_eq!(
        envelope(
            3,
            1500,
            Some("action"),
            Some(42),
            r#"{"device_id":"mm-1","event":"pick"}"#
        ),
        r#"{"v":1,"seq":3,"sim_ms":1500,"kind":"action","timestamp_ms":42,"device_id":"mm-1","event":"pick"}"#
    );
    assert_eq!(
        envelope(1, 0, None, None, "{}"),
        r#"{"v":1,"seq":1,"sim_ms":0,"kind":null}"#
    );

    let trace = r#"{"version": 1, "steps": [{"type": "delay", "duration_ms": 200}, {"type": "send", "payload": {"device_id": "mm-1", "event": "pick"}}]}"#;
    let (mut replay, _) = replay(
        &[("t.json", trace)],
        ReplayOptions {
            format: Format::Envelope,
            positions: false,
        },
    );
    let messages = play(&mut replay);
    let expected = format!(
        r#"{{"v":1,"seq":1,"sim_ms":200,"kind":"action","timestamp_ms":{},"device_id":"mm-1","event":"pick"}}"#,
        START_TS + 200
    );
    assert_eq!(messages[0].json, expected);
}

#[test]
fn a_payload_that_is_not_an_object_stops_the_replay_in_either_format() {
    let trace = r#"{"version": 1, "steps": [
      {"type": "send", "payload": {"device_id": "mm-1", "event": "pick"}},
      {"type": "send", "payload": [1, 2]}
    ]}"#;
    for format in [Format::Payload, Format::Envelope] {
        let options = ReplayOptions {
            format,
            positions: false,
        };
        let error = Replay::new(scenario(), traces(&[("t.json", trace)]), options, START_TS)
            .err()
            .unwrap();
        assert_eq!(
            error.to_string(),
            "1 problem: the traces use what the scenario doesn't define:\n\
             - t.json: step 2: payload is not a JSON object"
        );
    }
}

#[test]
fn position_reports_are_optional_and_only_while_moving() {
    let without = play(&mut sample_replay(ReplayOptions::default()));
    assert!(without.iter().all(|m| m.kind != Some("position")));

    let with = play(&mut sample_replay(ReplayOptions {
        format: Format::Payload,
        positions: true,
    }));
    let positions: Vec<&Message> = with.iter().filter(|m| m.kind == Some("position")).collect();
    assert_eq!(
        with.len() - positions.len(),
        without.len(),
        "the payloads are all still there"
    );
    let times: Vec<u64> = positions.iter().map(|m| m.sim_ms).collect();
    // Every 100 ms from the undock (1 s), through the move and the dock's glide, to the place (5 s),
    // where it reports once more on arrival. The move flows into the glide, so no stop at 4 s.
    let mut expected: Vec<u64> = (10..50).map(|i| i * 100).collect();
    expected.push(5000);
    assert_eq!(times, expected);
    assert_eq!(
        positions[5].json,
        r#"{"device_id":"mm-1","x":5.75,"y":4.567,"heading_rad":-2.0944,"speed_mps":1.0,"timestamp_ms":1001500}"#
    );
    let last: serde_json::Value = serde_json::from_str(&positions.last().unwrap().json).unwrap();
    assert_eq!(
        (last["x"].as_f64(), last["y"].as_f64(), last["speed_mps"].as_f64()),
        (Some(2.0), Some(5.0), Some(0.0))
    );
    // The arrival report comes before the place that follows the docking.
    let arrival = with
        .iter()
        .position(|m| m.kind == Some("position") && m.sim_ms == 5000)
        .unwrap();
    assert_eq!(with[arrival + 1].kind, Some("action"));
    assert!(with[arrival + 1].json.contains(r#""event":"place""#));

    // In the envelope format they are the simulator's own position events.
    let enveloped = play(&mut sample_replay(ReplayOptions {
        format: Format::Envelope,
        positions: true,
    }));
    let first = enveloped.iter().find(|m| m.kind == Some("position")).unwrap();
    let event: SimEvent = serde_json::from_str(&first.json).unwrap();
    assert_eq!(event.kind(), "position");
    assert_eq!((event.sim_ms, event.timestamp_ms), (1000, 1_001_000));
}

fn advance_to(replay: &mut Replay, sim_ms: u64) {
    while replay.state().sim_ms < sim_ms {
        replay.step();
    }
}

#[test]
fn mms_follow_their_undock_move_and_dock_events() {
    let mut replay = sample_replay(ReplayOptions::default());
    replay.start();
    let mm1 = |replay: &Replay| replay.state().mobile_manipulators[0].clone();

    let view = mm1(&replay);
    assert_eq!((view.x, view.y, view.at.as_deref()), (6.0, 5.0, Some("stocker")));

    // The first MM waits beside a machine 1 m out from its dock, leaning 30° to one side: straight
    // out from these docks is -90°, so it waits at -120°, i.e. (dock.x - 0.5, dock.y - 0.866).
    let out_y = 5.0 - 3f64.sqrt() / 2.0;
    let close = |view: &MmView, x: f64, y: f64| (view.x - x).abs() < 1e-9 && (view.y - y).abs() < 1e-9;

    // Undocking glides it out to the stocker's approach point over the next second.
    advance_to(&mut replay, 1500);
    let view = mm1(&replay);
    assert!(close(&view, 5.75, (5.0 + out_y) / 2.0), "{view:?}");
    assert!(
        view.at.is_none() && (view.speed_mps - 1.0).abs() < 1e-9,
        "{view:?}"
    );

    // The move drives between the approach points (5.5 -> 1.5), arriving at the dock event.
    advance_to(&mut replay, 3000);
    let view = mm1(&replay);
    assert!(close(&view, 3.5, out_y), "{view:?}");
    let motion = view.motion.unwrap();
    assert_eq!(
        (motion.target.at.as_deref(), motion.eta_ms),
        (Some("bonder"), 1000)
    );

    // Docking glides it into the bonder's dock (2, 5) over the next second.
    advance_to(&mut replay, 4500);
    let view = mm1(&replay);
    assert!(close(&view, 1.75, (out_y + 5.0) / 2.0), "{view:?}");
    assert_eq!(view.at.as_deref(), Some("bonder"));

    advance_to(&mut replay, 5000);
    let view = mm1(&replay);
    assert_eq!(
        (view.x, view.y, view.at.as_deref(), view.speed_mps),
        (2.0, 5.0, Some("bonder"), 0.0)
    );
    assert!(view.motion.is_none());

    // mm-2 has no events: it stays at its start.
    let mm2 = replay.state().mobile_manipulators[1].clone();
    assert_eq!((mm2.x, mm2.y, mm2.at), (10.0, 2.0, None));
}

#[test]
fn sensor_payloads_update_the_scenario_sensors() {
    let mut replay = sample_replay(ReplayOptions::default());
    replay.start();
    let reading = |replay: &Replay, id: &str| {
        let sensor = replay.state().sensors.into_iter().find(|s| s.id == id).unwrap();
        (sensor.last_value, sensor.last_ms)
    };
    assert_eq!(reading(&replay, "mm-1.vibration"), (Some(1.25), Some(0)));
    assert_eq!(reading(&replay, "mm-1.lidar"), (None, None));
    advance_to(&mut replay, 2500);
    assert_eq!(reading(&replay, "mm-1.lidar"), (Some(0.5), Some(2500)));
}

#[test]
fn nothing_can_be_done_by_hand() {
    let replay = sample_replay(ReplayOptions::default());
    let state = replay.state();
    assert!(
        state.mobile_manipulators[0]
            .actions
            .iter()
            .all(|a| !a.available && a.reason.as_deref() == Some(READ_ONLY))
    );
    let status = state.replay.unwrap();
    assert_eq!((status.total, status.duration_ms, status.done), (7, 5000, false));
    let mut replay = replay;
    let error = replay
        .control(Control::PlayScene("x".into()), START_TS)
        .unwrap_err();
    assert!(error.to_string().contains("no scenes"));
}

#[test]
fn pause_holds_and_reset_starts_again() {
    let mut replay = sample_replay(ReplayOptions::default());
    replay.start();
    advance_to(&mut replay, 1000);
    replay.control(Control::Pause, START_TS).unwrap();
    for _ in 0..100 {
        assert!(replay.step().is_empty());
    }
    assert_eq!(replay.state().sim_ms, 1000);
    replay.control(Control::Resume, START_TS).unwrap();

    let restarted = replay.control(Control::Reset, START_TS).unwrap();
    assert_eq!(restarted.len(), 2, "the payloads at offset 0 again");
    assert_eq!(restarted[0].seq, 4, "sequence numbers carry on");
    let state = replay.state();
    assert_eq!(
        (state.sim_ms, state.mobile_manipulators[0].at.as_deref()),
        (0, Some("stocker"))
    );
}

/// The bundled die-processing scenario and its traces, the real ones from Liminal Desk.
#[test]
fn the_die_processing_traces_fit_their_scenario() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../scenarios/replay-example/die-processing.toml");
    let scenario = Arc::new(crate::scenario::load_scenario(&path).unwrap());
    let traces = scenario.replay.as_ref().unwrap().traces.clone();
    let (mut replay, findings) =
        Replay::from_files(Arc::clone(&scenario), &traces, ReplayOptions::default(), START_TS).unwrap();
    assert_eq!(findings, vec![]);
    let status = replay.state().replay.unwrap();
    assert_eq!((status.total, status.duration_ms), (2680, 66_000));

    // At 40 s both MMs wait beside the stocker (mm-2 since 6 s, mm-1 since 38 s), apart.
    replay.start();
    advance_to(&mut replay, 40_000);
    let state = replay.state();
    let (mm1, mm2) = (&state.mobile_manipulators[0], &state.mobile_manipulators[1]);
    let apart = Vec2::new(mm1.x, mm1.y).distance(Vec2::new(mm2.x, mm2.y));
    assert!((apart - 1.0).abs() < 1e-6, "{apart} m apart: {mm1:?} {mm2:?}");
    assert_eq!((mm1.at.as_deref(), mm2.at.as_deref()), (None, None));
}

/// The bundled inspection cell: three short traces (one per MM, one for sensors) that use only what
/// the scenario defines.
#[test]
fn the_inspection_traces_fit_their_scenario() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../scenarios/replay-inspection/inspection-cell.toml");
    let scenario = Arc::new(crate::scenario::load_scenario(&path).unwrap());
    let traces = scenario.replay.as_ref().unwrap().traces.clone();
    assert_eq!(traces.len(), 3);
    let (mut replay, findings) =
        Replay::from_files(Arc::clone(&scenario), &traces, ReplayOptions::default(), START_TS).unwrap();
    assert_eq!(findings, vec![]);
    let status = replay.state().replay.unwrap();
    assert_eq!((status.total, status.duration_ms), (100, 40_000));

    // At the end each MM is back on its home dock, and the readings have reached the sensors.
    play(&mut replay);
    let state = replay.state();
    let at: Vec<Option<&str>> = state
        .mobile_manipulators
        .iter()
        .map(|mm| mm.at.as_deref())
        .collect();
    assert_eq!(at, vec![Some("loader"), Some("shelf")]);
    let value = |id: &str| state.sensors.iter().find(|s| s.id == id).unwrap().last_value;
    let temperature = value("inspector.temperature").unwrap();
    assert!((23.0..24.0).contains(&temperature), "cooled down: {temperature}");
    assert!(value("humidity").is_some());
}

fn messages_of(findings: &[Finding]) -> Vec<String> {
    findings.iter().map(Finding::to_string).collect()
}

/// The error `Replay::new` stops with, one line per problem.
fn errors_of(scenario: Arc<Scenario>, texts: &[(&str, &str)]) -> Vec<String> {
    let error = Replay::new(scenario, traces(texts), ReplayOptions::default(), START_TS)
        .err()
        .expect("the replay should be refused");
    error.to_string().lines().map(str::to_string).collect()
}

#[test]
fn anything_the_scenario_does_not_define_stops_the_replay_all_listed() {
    let trace = r#"{"version": 1, "steps": [
      {"type": "send", "payload": "hello"},
      {"type": "send", "payload": {"event": "pick"}},
      {"type": "send", "payload": {"device_id": "mm-9", "event": "pick"}},
      {"type": "send", "payload": {"device_id": "mm-1", "event": "weld"}},
      {"type": "send", "payload": {"device_id": "mm-1", "event": "undock", "target": "oven"}},
      {"type": "send", "payload": {"type": "temperature", "device_id": "mm-1", "value": 3}},
      {"type": "send", "payload": {"type": "vibration", "device_id": "mm-1", "value": 3, "unit": "g"}},
      {"type": "send", "payload": {"device_id": "mm-1"}},
      {"type": "send", "payload": {"device_id": "mm-1", "event": "move", "source": "stocker", "destination": "kiln"}}
    ]}"#;
    assert_eq!(
        errors_of(scenario(), &[("bad.json", trace)]),
        vec![
            "8 problems: the traces use what the scenario doesn't define:",
            "- bad.json: step 1: payload is not a JSON object",
            "- bad.json: step 2: payload has no device_id",
            "- bad.json: step 3: device 'mm-9' is not in the scenario",
            "- bad.json: step 4: event 'weld' is not one of the scenario's actions",
            "- bad.json: step 5: unknown machine 'oven'",
            "- bad.json: step 6: no 'temperature' sensor on 'mm-1' in the scenario",
            // Step 7 is fine: units aren't compared.
            "- bad.json: step 8: payload has neither an `event` nor a `type`, so it is neither an event nor a reading",
            "- bad.json: step 9: unknown machine 'kiln'",
        ]
    );
}

#[test]
fn dock_and_undock_must_be_defined_but_move_needs_no_action() {
    let without = SCENARIO
        .replace("[actions.dock]", "")
        .replace("[actions.undock]", "");
    let scenario = Arc::new(parse_scenario(&without, None).unwrap());
    let trace = r#"{"version": 1, "steps": [
      {"type": "send", "payload": {"device_id": "mm-1", "event": "undock", "target": "stocker"}},
      {"type": "send", "payload": {"device_id": "mm-1", "event": "move", "source": "stocker", "destination": "bonder"}},
      {"type": "send", "payload": {"device_id": "mm-1", "event": "dock", "target": "bonder"}}
    ]}"#;
    assert_eq!(
        errors_of(scenario, &[("t.json", trace)]),
        vec![
            "2 problems: the traces use what the scenario doesn't define:",
            "- t.json: step 1: event 'undock' is not one of the scenario's actions",
            "- t.json: step 3: event 'dock' is not one of the scenario's actions",
        ]
    );
    // With them defined, the same trace replays without a finding.
    let (_, findings) = replay(&[("t.json", trace)], ReplayOptions::default());
    assert_eq!(findings, vec![]);
}

#[test]
fn readings_of_machine_and_free_standing_sensors_are_found() {
    let text = format!(
        "{SCENARIO}\n[machines.bonder.sensors.temperature]\nunit = \"C\"\ndefault = 20.0\nrange = [0.0, 100.0]\n\n\
         [sensors.humidity]\nposition = [6.0, 1.0]\ndefault = 40.0\nrange = [0.0, 100.0]\n"
    );
    let scenario = Arc::new(parse_scenario(&text, None).unwrap());
    let trace = r#"{"version": 1, "steps": [
      {"type": "send", "payload": {"type": "temperature", "device_id": "bonder", "value": 25.5, "unit": "C"}},
      {"type": "send", "payload": {"type": "humidity", "device_id": "humidity", "value": 44.0, "unit": "%"}}
    ]}"#;
    let (mut replay, findings) = Replay::new(
        Arc::clone(&scenario),
        traces(&[("s.json", trace)]),
        ReplayOptions::default(),
        START_TS,
    )
    .unwrap();
    assert_eq!(findings, vec![]);
    replay.start();
    let value = |id: &str| {
        replay
            .state()
            .sensors
            .into_iter()
            .find(|s| s.id == id)
            .unwrap()
            .last_value
    };
    assert_eq!(
        (value("bonder.temperature"), value("humidity")),
        (Some(25.5), Some(44.0))
    );

    // On a machine that doesn't have it, it is an error.
    let wrong = r#"{"version": 1, "steps": [
      {"type": "send", "payload": {"type": "temperature", "device_id": "stocker", "value": 25.5}}
    ]}"#;
    assert_eq!(
        errors_of(scenario, &[("w.json", wrong)])[1],
        "- w.json: step 1: no 'temperature' sensor on 'stocker' in the scenario"
    );
}

#[test]
fn findings_report_events_that_do_not_follow_on() {
    let step = |payload: &str| {
        format!(r#"{{"type": "send", "payload": {payload}}}, {{"type": "delay", "duration_ms": 100}}"#)
    };
    let steps = [
        step(r#"{"device_id": "mm-1", "event": "dock", "target": "bonder"}"#),
        step(r#"{"device_id": "mm-1", "event": "dock", "target": "bonder"}"#),
        step(r#"{"device_id": "mm-1", "event": "undock", "target": "stocker"}"#),
        step(r#"{"device_id": "mm-1", "event": "move", "source": "bonder", "destination": "stocker"}"#),
        step(r#"{"device_id": "mm-1", "event": "dock", "target": "bonder"}"#),
        step(r#"{"device_id": "mm-1", "event": "move", "source": "bonder", "destination": "stocker"}"#),
    ];
    let trace = format!(r#"{{"version": 1, "steps": [{}]}}"#, steps.join(", "));
    let (_, findings) = replay(&[("seq.json", &trace)], ReplayOptions::default());
    assert_eq!(
        messages_of(&findings),
        vec![
            "seq.json: step 3: mm-1 docks at 'bonder' but is already docked at 'bonder'",
            "seq.json: step 5: mm-1 undocks from 'stocker' but is docked at 'bonder'",
            "seq.json: step 7: mm-1 moves from 'bonder' but was last at 'stocker'",
            "seq.json: step 9: mm-1 docks at 'bonder' but was moving to 'stocker'",
            "seq.json: step 11: mm-1 moves while docked at 'bonder' (no undock)",
        ]
    );
}

#[test]
fn findings_check_timestamps_against_the_delays_and_each_other() {
    let a = r#"{"version": 1, "steps": [
      {"type": "send", "payload": {"device_id": "mm-1", "event": "pick", "timestamp_ms": 1000}},
      {"type": "delay", "duration_ms": 100},
      {"type": "send", "payload": {"device_id": "mm-1", "event": "pick", "timestamp_ms": 1100}},
      {"type": "delay", "duration_ms": 100},
      {"type": "send", "payload": {"device_id": "mm-1", "event": "pick", "timestamp_ms": 1250}}
    ]}"#;
    let b = r#"{"version": 1, "steps": [
      {"type": "send", "payload": {"type": "vibration", "device_id": "mm-1", "value": 1, "unit": "m/s^2", "timestamp_ms": 5000}}
    ]}"#;
    let (_, findings) = replay(&[("a.json", a), ("b.json", b)], ReplayOptions::default());
    assert_eq!(
        messages_of(&findings),
        vec![
            "a.json: step 5: timestamp_ms disagrees with the trace's delays",
            "b.json: starts at timestamp_ms 5000, 4000 ms after a.json (1000); the replay plays both from 0",
        ]
    );
}

#[test]
fn repeated_findings_are_grouped() {
    // A warning: every timestamp after the first disagrees with the delays.
    let steps: Vec<String> = (0..5)
        .map(|_| {
            r#"{"type": "send", "payload": {"device_id": "mm-1", "event": "pick", "timestamp_ms": 1000}}"#
                .to_string()
        })
        .collect();
    let trace = format!(
        r#"{{"version": 1, "steps": [{}]}}"#,
        steps.join(r#", {"type": "delay", "duration_ms": 100}, "#)
    );
    let (_, findings) = replay(&[("g.json", &trace)], ReplayOptions::default());
    assert_eq!(
        messages_of(&findings),
        vec!["g.json: timestamp_ms disagrees with the trace's delays (4 times: steps 3, 5, 7, …)"]
    );

    // An error: the same undefined action three times.
    let send = r#"{"type": "send", "payload": {"device_id": "mm-1", "event": "weld"}}"#;
    let trace = format!(r#"{{"version": 1, "steps": [{}]}}"#, [send; 3].join(", "));
    assert_eq!(
        errors_of(scenario(), &[("w.json", &trace)]),
        vec![
            "3 problems: the traces use what the scenario doesn't define:",
            "- w.json: event 'weld' is not one of the scenario's actions (3 times: steps 1, 2, 3)",
        ]
    );
}
