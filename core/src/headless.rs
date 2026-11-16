//! Running a scenario as fast as possible, without the real-time runtime: for recordings, test
//! data and tests. Events are stamped as if the run had happened in real time from `start_ts`.

use crate::command::Control;
use crate::engine::Engine;
use crate::event::{EventBody, SceneState, SimEvent};
use crate::runtime::Driver;
use crate::scenario::Scenario;
use crate::scene::Repeat;
use crate::sinks::{EventSink, ToMessage};
use anyhow::{Context, bail};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct FastRun {
    /// Scene to play from the start. The run ends when it finishes or fails.
    pub scene: Option<String>,
    /// Stop after this much simulation time (required unless the scene ends by itself).
    pub max_sim_ms: Option<u64>,
    /// Wall-clock time (epoch ms) of simulation time 0: events get `ts = start_ts + sim_ms`.
    pub start_ts: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The scene finished.
    Finished,
    /// The scene failed; the error is the rejected step's reason.
    Failed(String),
    /// `max_sim_ms` was reached first.
    TimeLimit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FastRunSummary {
    pub sim_ms: u64,
    pub events: u64,
    pub outcome: Outcome,
}

/// Steps a fresh engine for `scenario` until the scene ends or the time limit is reached, writing
/// every event to `sinks` (which are closed at the end).
pub fn run_fast(
    scenario: Arc<Scenario>,
    run: &FastRun,
    sinks: &mut [Box<dyn EventSink>],
) -> anyhow::Result<FastRunSummary> {
    match &run.scene {
        Some(id) => {
            let index = scenario
                .scene_index(id)
                .with_context(|| format!("Unknown scene '{id}'"))?;
            if run.max_sim_ms.is_none() && scenario.scenes[index].repeat == Repeat::Forever {
                bail!("Scene '{id}' loops forever: give a duration");
            }
        }
        None if run.max_sim_ms.is_none() => {
            bail!("Nothing would end the run: give a scene or a duration")
        }
        None => {}
    }

    let mut engine = Engine::new(scenario);
    let controls = run
        .scene
        .iter()
        .map(|id| Control::PlayScene(id.clone()))
        .collect();
    let (events, outcome) = drive(&mut engine, run.start_ts, controls, sinks, |engine, batch| {
        scene_outcome(batch).or_else(|| {
            run.max_sim_ms
                .is_some_and(|max| engine.sim_ms() >= max)
                .then_some(Outcome::TimeLimit)
        })
    })?;
    Ok(FastRunSummary {
        sim_ms: engine.sim_ms(),
        events,
        outcome,
    })
}

/// Plays `driver` from its start as fast as possible, writing every batch to `sinks` (closed at the
/// end). Events are stamped as if it ran in real time from `start_ts`. `controls` are applied right
/// after the start (e.g. playing a scene). `until` is asked after each batch whether the run is
/// over; its answer is returned with the number of items written.
pub fn drive<D: Driver, R>(
    driver: &mut D,
    start_ts: u64,
    controls: Vec<Control>,
    sinks: &mut [Box<dyn EventSink>],
    mut until: impl FnMut(&D, &[D::Item]) -> Option<R>,
) -> anyhow::Result<(u64, R)> {
    let tick_ms = driver.scenario().sim.tick_ms;
    let mut written = 0u64;
    let mut write = |driver: &D, batch: Vec<D::Item>| -> anyhow::Result<Option<R>> {
        let batch: Arc<[D::Item]> = batch.into();
        if !batch.is_empty() {
            let messages = D::Item::to_messages(&batch);
            for sink in sinks.iter_mut() {
                sink.write_batch(&messages)?;
            }
            written += batch.len() as u64;
        }
        Ok(until(driver, &batch))
    };

    let batch = driver.start(start_ts);
    let mut ended = write(driver, batch)?;
    for control in controls {
        let batch = driver.control(control, start_ts)?;
        ended = write(driver, batch)?.or(ended);
    }
    let end = loop {
        if let Some(end) = ended.take() {
            break end;
        }
        let ts = start_ts + driver.sim_ms() + tick_ms;
        let batch = driver.step(ts);
        ended = write(driver, batch)?;
    };

    for sink in sinks.iter_mut() {
        sink.close()?;
    }
    Ok((written, end))
}

/// `Finished` / `Failed` if this batch ends the scene.
pub fn scene_outcome(batch: &[SimEvent]) -> Option<Outcome> {
    batch.iter().find_map(|event| match &event.body {
        EventBody::Scene {
            state: SceneState::Finished,
            ..
        } => Some(Outcome::Finished),
        EventBody::Scene {
            state: SceneState::Failed,
            error,
            ..
        } => Some(Outcome::Failed(error.clone().unwrap_or_default())),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::load_scenario;
    use crate::sinks::Message;
    use std::path::Path;
    use std::sync::Mutex;

    /// Collects events in memory, parsed back from what a sink is given.
    struct MemorySink(Arc<Mutex<Vec<SimEvent>>>);

    impl EventSink for MemorySink {
        fn write_batch(&mut self, messages: &[Message]) -> std::io::Result<()> {
            let events = messages
                .iter()
                .map(|message| serde_json::from_str(&message.json).unwrap());
            self.0.lock().unwrap().extend(events);
            Ok(())
        }
    }

    fn default_scenario() -> Arc<Scenario> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scenarios/sample-floor.toml");
        Arc::new(load_scenario(&path).unwrap())
    }

    fn run_collecting(run: &FastRun) -> anyhow::Result<(FastRunSummary, Vec<SimEvent>)> {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut sinks: Vec<Box<dyn EventSink>> = vec![Box::new(MemorySink(Arc::clone(&events)))];
        let summary = run_fast(default_scenario(), run, &mut sinks)?;
        let events = events.lock().unwrap().clone();
        Ok((summary, events))
    }

    #[test]
    fn the_default_production_scene_loops_without_failing() {
        let (summary, events) = run_collecting(&FastRun {
            scene: Some("production".into()),
            max_sim_ms: Some(120_000),
            start_ts: 1_000_000,
        })
        .unwrap();
        assert_eq!(summary.outcome, Outcome::TimeLimit);
        assert_eq!(summary.sim_ms, 120_000);
        assert_eq!(summary.events, events.len() as u64);

        let states: Vec<SceneState> = events
            .iter()
            .filter_map(|e| match &e.body {
                EventBody::Scene { state, .. } => Some(*state),
                _ => None,
            })
            .collect();
        assert_eq!(states[0], SceneState::Started);
        assert!(states.contains(&SceneState::Restarted), "{states:?}");
        assert!(!states.contains(&SceneState::Failed), "{events:#?}");
        assert!(events.iter().any(|e| e.kind() == "action"));

        // Stamped as if it ran in real time; sequence numbers are contiguous.
        assert!(events.iter().all(|e| e.timestamp_ms == 1_000_000 + e.sim_ms));
        let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, (1..=events.len() as u64).collect::<Vec<_>>());
    }

    /// Scenes are only checked for dangling references when they load. An action the MM isn't in a
    /// position to perform only fails when it is reached, so play them all.
    #[test]
    fn every_bundled_scene_plays_without_failing() {
        let mut played = 0;
        for path in crate::test_support::bundled_scenarios() {
            let scenario = Arc::new(load_scenario(&path).unwrap());
            for scene in &scenario.scenes {
                let events = Arc::new(Mutex::new(Vec::new()));
                let mut sinks: Vec<Box<dyn EventSink>> = vec![Box::new(MemorySink(Arc::clone(&events)))];
                let run = FastRun {
                    scene: Some(scene.id.clone()),
                    max_sim_ms: Some(300_000),
                    start_ts: 0,
                };
                run_fast(Arc::clone(&scenario), &run, &mut sinks).unwrap();
                let events = events.lock().unwrap();
                let failure = events.iter().find(|event| {
                    matches!(
                        &event.body,
                        EventBody::Scene {
                            state: SceneState::Failed,
                            ..
                        }
                    )
                });
                assert!(
                    failure.is_none(),
                    "{}, scene '{}': {failure:?}",
                    path.display(),
                    scene.id
                );
                played += 1;
            }
        }
        assert!(played > 0, "no scenes found in the bundled scenarios");
    }

    #[test]
    fn a_one_shot_scene_ends_the_run_when_it_finishes() {
        let (summary, events) = run_collecting(&FastRun {
            scene: Some("vibration_fault".into()),
            max_sim_ms: None,
            start_ts: 0,
        })
        .unwrap();
        assert_eq!(summary.outcome, Outcome::Finished);
        assert_eq!(events.last().unwrap().kind(), "scene");
    }

    #[test]
    fn refuses_runs_that_would_never_end() {
        let looping = run_collecting(&FastRun {
            scene: Some("production".into()),
            max_sim_ms: None,
            start_ts: 0,
        })
        .unwrap_err();
        assert!(looping.to_string().contains("loops forever"));
        let nothing = run_collecting(&FastRun {
            scene: None,
            max_sim_ms: None,
            start_ts: 0,
        })
        .unwrap_err();
        assert!(nothing.to_string().contains("give a scene or a duration"));
        let unknown = run_collecting(&FastRun {
            scene: Some("nope".into()),
            max_sim_ms: Some(1000),
            start_ts: 0,
        })
        .unwrap_err();
        assert!(unknown.to_string().contains("Unknown scene 'nope'"));
    }
}
