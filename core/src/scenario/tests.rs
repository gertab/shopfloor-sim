use super::*;
use crate::replay::Format;

const MINIMAL: &str = r#"
    name = "Test cell"

    [floor]
    width_m = 10.0
    height_m = 6.0

    [machines.machine1]
    position = [2.0, 4.5]
    size = [2.0, 1.0]

    [machines.machine2]
    label = "Press"
    position = [8.0, 4.5]
    size = [2.0, 1.0]
    dock = [8.0, 3.0]

    [mobile_manipulators.mm1]
    start_at = "machine1"
    max_speed_mps = 1.0

    [mobile_manipulators.mm1.sensors.vibration]
    unit = "g"
    default = 0.1
    range = [0.0, 2.0]
    period_ms = 500
"#;

fn errors_of(text: &str) -> String {
    format!("{:#}", parse_scenario(text, None).unwrap_err())
}

#[test]
fn parses_minimal_scenario_with_defaults() {
    let scenario = parse_scenario(MINIMAL, None).unwrap();
    assert_eq!(scenario.name, "Test cell");
    assert_eq!(scenario.sim, SimSettings::default());
    assert!(!scenario.sim.start_paused);
    assert_eq!(scenario.machines.len(), 2);
    assert_eq!(scenario.machines[0].label, "machine1");
    // Default dock: centred, 0.5 m below the footprint.
    assert_eq!(scenario.machines[0].dock, Vec2::new(2.0, 3.5));
    assert_eq!(scenario.machines[1].dock, Vec2::new(8.0, 3.0));

    let mm = &scenario.mobile_manipulators[0];
    assert_eq!(mm.start, Vec2::new(2.0, 3.5));
    assert_eq!(mm.radius_m, 0.35);
    assert_eq!(mm.sensors, vec!["mm1.vibration".to_string()]);

    let sensor = &scenario.sensors[0];
    assert_eq!(sensor.id, "mm1.vibration");
    assert_eq!(sensor.kind, "vibration");
    assert_eq!(sensor.label, "Vibration");
    assert_eq!(sensor.host, "mm1");
    assert_eq!(sensor.host_kind, HostKind::MobileManipulator);
    assert_eq!((sensor.min, sensor.max), (0.0, 2.0));
    assert_eq!(scenario.output.position_interval_ms, 100);
}

#[test]
fn rejects_unknown_keys() {
    let text = MINIMAL.replace("max_speed_mps = 1.0", "max_sped_mps = 1.0");
    assert!(errors_of(&text).contains("max_sped_mps"));
}

#[test]
fn rejects_shared_ids_between_machines_and_mms() {
    let text = format!("{MINIMAL}\n[mobile_manipulators.machine2]\nstart = [5.0, 1.0]\n");
    assert!(errors_of(&text).contains("id 'machine2' is used more than once"));
}

#[test]
fn rejects_bad_sensor_ranges_and_periods() {
    let text = MINIMAL
        .replace("default = 0.1", "default = 5.0")
        .replace("period_ms = 500", "period_ms = 5");
    let errors = errors_of(&text);
    assert!(errors.contains("default 5 is outside range"), "{errors}");
    assert!(
        errors.contains("period_ms (5) must be >= sim.tick_ms (20)"),
        "{errors}"
    );
}

#[test]
fn rejects_unknown_start_machine_and_out_of_bounds_layout() {
    let text = MINIMAL
        .replace("start_at = \"machine1\"", "start_at = \"machine9\"")
        .replace("position = [8.0, 4.5]", "position = [9.5, 4.5]");
    let errors = errors_of(&text);
    assert!(errors.contains("unknown machine 'machine9'"), "{errors}");
    assert!(
        errors.contains("machines.machine2 footprint lies outside"),
        "{errors}"
    );
}

#[test]
fn requires_exactly_one_start() {
    let text = MINIMAL.replace(
        "start_at = \"machine1\"",
        "start_at = \"machine1\"\nstart = [1.0, 1.0]",
    );
    assert!(errors_of(&text).contains("exactly one of `start` or `start_at`"));
}

#[test]
fn resolves_relative_sink_paths_against_scenario_file() {
    let text = format!(
        "{MINIMAL}\n[output.sinks.rec]\ntype = \"jsonl\"\npath = \"../recordings/a.jsonl\"\n\n[output.sinks.term]\ntype = \"stdout\"\n"
    );
    let scenario = parse_scenario(&text, Some(Path::new("/x/scenarios/s.toml"))).unwrap();
    assert_eq!(
        scenario.output.sinks,
        vec![
            SinkDef {
                name: "rec".into(),
                kind: SinkKind::Jsonl {
                    path: PathBuf::from("/x/scenarios/../recordings/a.jsonl"),
                    append: true,
                },
            },
            SinkDef {
                name: "term".into(),
                kind: SinkKind::Stdout,
            },
        ]
    );
}

#[test]
fn parses_tcp_sinks() {
    let text = format!("{MINIMAL}\n[output.sinks.tcp]\ntype = \"tcp\"\nbind = \"127.0.0.1:9000\"\n");
    let scenario = parse_scenario(&text, None).unwrap();
    assert_eq!(
        scenario.output.sinks,
        vec![SinkDef {
            name: "tcp".into(),
            kind: SinkKind::Tcp {
                bind: "127.0.0.1:9000".into()
            },
        }]
    );
}

#[test]
fn rejects_unknown_sink_type() {
    let text = format!("{MINIMAL}\n[output.sinks.x]\ntype = \"carrier_pigeon\"\n");
    assert!(errors_of(&text).contains("carrier_pigeon"));
}

#[test]
fn parses_mqtt_sinks() {
    let text = format!(
        "{MINIMAL}
[output.sinks.mqtt]
type = \"mqtt\"
broker = \"mqtt://127.0.0.1:1883\"

[[output.sinks.mqtt.topics]]
topic = \"shopfloor/sensors\"
kinds = [\"sensor\", \"sensor_set\"]

[[output.sinks.mqtt.topics]]
topic = \"shopfloor/mm1\"
entities = [\"mm1\"]
"
    );
    let scenario = parse_scenario(&text, None).unwrap();
    assert_eq!(
        scenario.output.sinks,
        vec![SinkDef {
            name: "mqtt".into(),
            kind: SinkKind::Mqtt {
                broker: "mqtt://127.0.0.1:1883".into(),
                topics: vec![
                    MqttTopic {
                        topic: "shopfloor/sensors".into(),
                        kinds: vec!["sensor".into(), "sensor_set".into()],
                        entities: vec![],
                    },
                    MqttTopic {
                        topic: "shopfloor/mm1".into(),
                        kinds: vec![],
                        entities: vec!["mm1".into()],
                    },
                ],
                qos: 0,
                client_id: None,
            },
        }]
    );
    assert!(scenario.output.sinks[0].kind.is_live());
}

#[test]
fn rejects_invalid_mqtt_sinks() {
    let topic = "[[output.sinks.m.topics]]\ntopic = \"t\"\n";
    let cases = [
        (format!("broker = \"localhost:99999\"\n{topic}"), "invalid port"),
        (format!("broker = \"mqtt://\"\n{topic}"), "not a broker address"),
        (
            format!("broker = \"localhost\"\nqos = 3\n{topic}"),
            "qos must be 0, 1 or 2",
        ),
        (
            "broker = \"localhost\"\n".to_string(),
            "must list at least one topic",
        ),
        (
            "broker = \"localhost\"\n[[output.sinks.m.topics]]\ntopic = \"a/#\"\n".to_string(),
            "'a/#' contains a wildcard",
        ),
        (
            format!("broker = \"localhost\"\n{topic}{topic}"),
            "'t' is listed more than once",
        ),
        (
            format!("broker = \"localhost\"\n{topic}kinds = [\"sensors\"]\n"),
            "unknown event kind 'sensors'",
        ),
        (
            format!("broker = \"localhost\"\n{topic}entities = [\"mm9\"]\n"),
            "unknown entity 'mm9'",
        ),
    ];
    for (sink, expected) in cases {
        let text = format!("{MINIMAL}\n[output.sinks.m]\ntype = \"mqtt\"\n{sink}");
        let errors = errors_of(&text);
        assert!(errors.contains(expected), "expected {expected:?} in:\n{errors}");
    }
}

#[test]
fn parses_broker_addresses() {
    assert_eq!(parse_broker("localhost"), Ok(("localhost".into(), 1883)));
    assert_eq!(
        parse_broker("mqtt://10.0.0.5:1884"),
        Ok(("10.0.0.5".into(), 1884))
    );
    assert_eq!(
        parse_broker("broker.local:8883"),
        Ok(("broker.local".into(), 8883))
    );
    assert!(parse_broker("localhost:0").is_err());
    assert!(parse_broker("").is_err());
}

#[test]
fn machine_docked_at_uses_tolerance() {
    let scenario = parse_scenario(MINIMAL, None).unwrap();
    assert_eq!(
        scenario
            .machine_docked_at(Vec2::new(2.1, 3.6))
            .map(|m| m.id.as_str()),
        Some("machine1")
    );
    assert!(scenario.machine_docked_at(Vec2::new(5.0, 3.5)).is_none());
}

#[test]
fn actions_default_to_all_and_can_be_limited_per_mm() {
    let text = format!(
        "{MINIMAL}\n[actions.pick]\nrequires_machine = true\n\n[actions.scan]\nlabel = \"Scan area\"\nallowed_while_moving = true\n\n[mobile_manipulators.mm2]\nstart = [5.0, 1.0]\nactions = [\"scan\"]\n"
    );
    let scenario = parse_scenario(&text, None).unwrap();
    let pick = scenario.action("pick").unwrap();
    assert_eq!(pick.label, "Pick");
    assert!(pick.requires_machine && !pick.allowed_while_moving);
    let scan = scenario.action("scan").unwrap();
    assert_eq!(scan.label, "Scan area");
    assert!(!scan.requires_machine && scan.allowed_while_moving);
    assert_eq!(scenario.mobile_manipulators[0].actions, vec!["pick", "scan"]);
    assert_eq!(scenario.mobile_manipulators[1].actions, vec!["scan"]);
}

#[test]
fn rejects_unknown_or_duplicate_mm_actions() {
    let text = MINIMAL.replace(
        "max_speed_mps = 1.0",
        "max_speed_mps = 1.0\nactions = [\"pick\", \"pick\", \"fly\"]",
    );
    let text = format!("{text}\n[actions.pick]\n");
    let errors = errors_of(&text);
    assert!(errors.contains("unknown action 'fly' (known: pick)"), "{errors}");
    assert!(errors.contains("lists 'pick' twice"), "{errors}");
}

#[test]
fn parses_an_optional_replay_section() {
    assert!(parse_scenario(MINIMAL, None).unwrap().replay.is_none());

    let text = format!(
        "{MINIMAL}\n[replay]\ntraces = [\"traces/a.json\", \"/abs/b.json\"]\nformat = \"envelope\"\n"
    );
    let scenario = parse_scenario(&text, Some(Path::new("/project/scenarios/cell.toml"))).unwrap();
    assert_eq!(
        scenario.replay,
        Some(ReplaySettings {
            traces: vec![
                PathBuf::from("/project/scenarios/traces/a.json"),
                PathBuf::from("/abs/b.json"),
            ],
            format: Format::Envelope,
            positions: false,
        })
    );

    let errors = errors_of(&format!("{MINIMAL}\n[replay]\ntraces = []\n"));
    assert!(
        errors.contains("replay.traces must list at least one trace file"),
        "{errors}"
    );
    let error = format!(
        "{:#}",
        parse_scenario(
            &format!("{MINIMAL}\n[replay]\ntraces = [\"a\"]\nformat = \"xml\"\n"),
            None
        )
        .unwrap_err()
    );
    assert!(error.contains("unknown variant `xml`"), "{error}");
}

/// Every bundled scenario still loads, including those in their own folder.
#[test]
fn the_bundled_scenarios_load() {
    fn load_all(dir: &Path, count: &mut usize) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            // Liminal configs that go with a scenario (`liminal*.toml`) aren't scenarios.
            let liminal = path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("liminal"));
            if path.is_dir() {
                load_all(&path, count);
            } else if path.extension().is_some_and(|ext| ext == "toml") && !liminal {
                load_scenario(&path).unwrap_or_else(|error| panic!("{}: {error:#}", path.display()));
                *count += 1;
            }
        }
    }
    let mut count = 0;
    load_all(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../scenarios"),
        &mut count,
    );
    assert!(count >= 7, "found {count} scenarios");
}

#[test]
fn rejects_reserved_action_ids() {
    let errors = errors_of(&format!("{MINIMAL}\n[actions.dock]\n\n[actions.pick]\n"));
    assert!(errors.contains("action 'dock' is reserved"), "{errors}");
    assert!(!errors.contains("'pick'"), "{errors}");
}

#[test]
fn parses_free_standing_sensors() {
    let text = format!(
        "{MINIMAL}\n[sensors.hall_temp]\nkind = \"temperature\"\nunit = \"C\"\nposition = [4.0, 5.0]\ndefault = 22.0\nrange = [0.0, 50.0]\n"
    );
    let scenario = parse_scenario(&text, None).unwrap();
    let sensor = scenario.sensors.iter().find(|s| s.id == "hall_temp").unwrap();
    assert_eq!(sensor.host, "hall_temp");
    assert_eq!(sensor.host_kind, HostKind::Floor);
    assert_eq!(sensor.kind, "temperature");
    assert_eq!(sensor.label, "Hall temp");
    assert_eq!(sensor.position, Some(Vec2::new(4.0, 5.0)));
    // Mounted sensors have no position of their own.
    assert_eq!(scenario.sensors[0].position, None);
}

#[test]
fn rejects_bad_sensor_positions() {
    let text = format!(
        "{MINIMAL}\n[sensors.nowhere]\ndefault = 1.0\nrange = [0.0, 2.0]\n\n[sensors.outside]\nposition = [11.0, 1.0]\ndefault = 1.0\nrange = [0.0, 2.0]\n\n[sensors.machine1]\nposition = [1.0, 1.0]\ndefault = 1.0\nrange = [0.0, 2.0]\n"
    )
    .replace("period_ms = 500", "period_ms = 500\nposition = [1.0, 1.0]");
    let errors = errors_of(&text);
    assert!(
        errors.contains("sensor 'nowhere': free-standing sensors need a `position"),
        "{errors}"
    );
    assert!(
        errors.contains("sensor 'outside': position (11.00, 1.00) is outside the floor"),
        "{errors}"
    );
    assert!(
        errors.contains("id 'machine1' is used more than once"),
        "{errors}"
    );
    assert!(
        errors.contains("sensor 'mm1.vibration': `position` is only for free-standing"),
        "{errors}"
    );
}

#[test]
fn bundled_scenarios_are_valid() {
    for path in crate::test_support::bundled_scenarios() {
        load_scenario(&path).unwrap_or_else(|e| panic!("{}: {e:#}", path.display()));
    }
}
