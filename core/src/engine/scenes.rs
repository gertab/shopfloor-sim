//! Scene playback: the engine plays its scene itself, after each tick's physics, so a scene pauses,
//! resets and runs fast together with the world.

use super::{Engine, known};
use crate::command::{CommandEnvelope, Origin};
use crate::event::{EventBody, SceneState, SimEvent};
use crate::scene::{Repeat, ScenePlayer, SceneStatus, Step, TrackStatus, Wait, format_seconds};
use anyhow::anyhow;
use std::sync::Arc;

impl Engine {
    pub(super) fn play_scene(&mut self, id: &str, ts: u64) -> anyhow::Result<Vec<SimEvent>> {
        let scenario = Arc::clone(&self.scenario);
        let index = scenario.scene_index(id).ok_or_else(|| {
            let ids = known(scenario.scenes.iter().map(|s| s.id.as_str()));
            anyhow!(
                "Unknown scene '{id}' (known: {})",
                if ids.is_empty() { "none" } else { ids.as_str() }
            )
        })?;
        let mut events = self.stop_scene(ts);
        let player = ScenePlayer::new(index, scenario.scenes[index].tracks.len(), self.sim_ms);
        let body = scene_body(id, player.iteration, SceneState::Started, None);
        events.push(self.event(ts, body));
        self.scene = Some(player);
        events.extend(self.run_scene(ts));
        Ok(events)
    }

    pub(super) fn stop_scene(&mut self, ts: u64) -> Vec<SimEvent> {
        let Some(player) = self.scene.take() else {
            return Vec::new();
        };
        let id = self.scenario.scenes[player.scene].id.clone();
        let body = scene_body(&id, player.iteration, SceneState::Stopped, None);
        vec![self.event(ts, body)]
    }

    /// Runs every track of the playing scene until it reaches a wait that isn't over (or its end),
    /// then finishes or restarts the scene when all tracks are done.
    pub(super) fn run_scene(&mut self, ts: u64) -> Vec<SimEvent> {
        // Taken out while running, so commands go through `apply` like any other command.
        let Some(mut player) = self.scene.take() else {
            return Vec::new();
        };
        let scenario = Arc::clone(&self.scenario);
        let def = &scenario.scenes[player.scene];
        let mut events = Vec::new();

        loop {
            for (track, cursor) in def.tracks.iter().zip(player.tracks.iter_mut()) {
                loop {
                    if let Some(wait) = cursor.waiting {
                        let over = match wait {
                            Wait::Until(until_ms) => self.sim_ms >= until_ms,
                            Wait::Idle(mm) => self.mms[mm].motion.is_none(),
                        };
                        if !over {
                            break;
                        }
                        cursor.waiting = None;
                    }
                    let Some(step) = track.steps.get(cursor.next) else {
                        break;
                    };
                    cursor.next += 1;
                    match step {
                        Step::Command(command) => {
                            let envelope = CommandEnvelope::new(Origin::Scene, command.clone());
                            match self.apply(envelope, ts) {
                                Ok(batch) => events.extend(batch),
                                Err(error) => {
                                    // The scene stops; the world keeps running.
                                    let body = scene_body(
                                        &def.id,
                                        player.iteration,
                                        SceneState::Failed,
                                        Some(format!("{error:#}")),
                                    );
                                    events.push(self.event(ts, body));
                                    return events;
                                }
                            }
                        }
                        Step::WaitMs(ms) => cursor.waiting = Some(Wait::Until(self.sim_ms + ms)),
                        Step::WaitIdle(mm) => {
                            let index = scenario.mm_index(mm).expect("validated: scene MMs exist");
                            cursor.waiting = Some(Wait::Idle(index));
                        }
                    }
                }
            }

            let iteration_done = def
                .tracks
                .iter()
                .zip(&player.tracks)
                .all(|(track, cursor)| cursor.waiting.is_none() && cursor.next >= track.steps.len());
            if !iteration_done {
                break;
            }
            let another = match def.repeat {
                Repeat::Once => false,
                Repeat::Times(times) => player.iteration < times,
                Repeat::Forever => true,
            };
            if !another {
                let body = scene_body(&def.id, player.iteration, SceneState::Finished, None);
                events.push(self.event(ts, body));
                return events;
            }
            // At most one iteration starts per tick, so a scene whose waits are already over
            // can't spin within a single tick; it restarts on the next one.
            if player.iteration_started_ms == self.sim_ms {
                break;
            }
            player.restart(self.sim_ms);
            let body = scene_body(&def.id, player.iteration, SceneState::Restarted, None);
            events.push(self.event(ts, body));
        }

        self.scene = Some(player);
        events
    }

    pub(super) fn scene_status(&self) -> Option<SceneStatus> {
        let player = self.scene.as_ref()?;
        let def = &self.scenario.scenes[player.scene];
        Some(SceneStatus {
            id: def.id.clone(),
            label: def.label.clone(),
            iteration: player.iteration,
            tracks: def
                .tracks
                .iter()
                .zip(&player.tracks)
                .map(|(track, cursor)| {
                    let current = cursor.waiting.map(|wait| {
                        let text = track.steps[cursor.next - 1].describe(&self.scenario);
                        match wait {
                            Wait::Until(until_ms) => format!(
                                "{text} ({} left)",
                                format_seconds(until_ms.saturating_sub(self.sim_ms))
                            ),
                            Wait::Idle(_) => text,
                        }
                    });
                    TrackStatus {
                        name: track.name.clone(),
                        step: cursor.next.min(track.steps.len()),
                        steps: track.steps.len(),
                        done: current.is_none() && cursor.next >= track.steps.len(),
                        current,
                    }
                })
                .collect(),
        })
    }
}

fn scene_body(id: &str, iteration: u32, state: SceneState, error: Option<String>) -> EventBody {
    EventBody::Scene {
        scene: id.to_string(),
        state,
        iteration,
        error,
    }
}
