import { Play, Repeat as RepeatIcon, Square } from "lucide-react";
import { repeatLabel } from "../../format";
import type { Control, Scenario, WorldState } from "../../types";

interface ScenesPanelProps {
  scenario: Scenario;
  world: WorldState | null;
  onControl: (control: Control) => void;
}

/** The scenario's scenes: play / stop, and live progress of the playing one. */
export function ScenesPanel({ scenario, world, onControl }: ScenesPanelProps) {
  const playing = world?.scene ?? null;
  return (
    <section className="panel-section">
      <h2>Scenes</h2>
      {scenario.scenes.length === 0 && (
        <p className="hint">This scenario has no scenes. Add [scenes.&lt;id&gt;] tables to script runs.</p>
      )}
      {scenario.scenes.map((scene) => {
        const status = playing?.id === scene.id ? playing : null;
        const steps = scene.tracks.reduce((total, track) => total + track.steps.length, 0);
        return (
          <div key={scene.id} className={`scene-card${status ? " is-playing" : ""}`}>
            <div className="scene-header">
              <strong title={scene.id}>{scene.label}</strong>
              <span className="scene-repeat" title="How often the scene runs">
                {scene.repeat !== "once" && <RepeatIcon size={11} />}
                {repeatLabel(scene.repeat)}
              </span>
              {status ? (
                <button
                  type="button"
                  className="chip-button danger"
                  title="Stop the scene (the world keeps running)"
                  onClick={() => onControl("stop_scene")}
                >
                  <Square size={11} />
                  Stop
                </button>
              ) : (
                <button
                  type="button"
                  className="chip-button scene-play"
                  title={playing ? `Stop '${playing.label}' and play this scene` : "Play this scene"}
                  onClick={() => onControl({ play_scene: scene.id })}
                >
                  <Play size={11} />
                  Play
                </button>
              )}
            </div>
            {status ? (
              <div className="scene-tracks">
                {status.tracks.map((track) => (
                  <div key={track.name} className={`scene-track${track.done ? " is-done" : ""}`}>
                    <span className="scene-track-name">{track.name}</span>
                    <span className="scene-track-step">
                      {track.step}/{track.steps}
                    </span>
                    <span className="scene-track-current">
                      {track.done ? "done, waiting for the other tracks" : track.current}
                    </span>
                  </div>
                ))}
                <div className="muted small">
                  iteration {status.iteration}
                  {world?.paused ? " · paused" : ""}
                </div>
              </div>
            ) : (
              <div className="muted small">
                {scene.tracks.length} {scene.tracks.length === 1 ? "track" : "tracks"} · {steps} steps
              </div>
            )}
          </div>
        );
      })}
      {playing && <p className="hint">Manual control still works while a scene plays.</p>}
    </section>
  );
}
