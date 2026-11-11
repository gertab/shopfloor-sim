//! `shopfloor`: run a scenario (and optionally one of its scenes) without the GUI.
//!
//! ```text
//! shopfloor check scenarios/sample-floor.toml
//! shopfloor run scenarios/sample-floor.toml --scene production --duration 300
//! shopfloor run scenarios/sample-floor.toml --scene production --fast --duration 600 --jsonl out.jsonl
//! ```
//!
//! Events go to the scenario's `[output.sinks]` (plus `--jsonl`); logs go to stderr.

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use shopfloor_core::command::Control;
use shopfloor_core::headless::{FastRun, Outcome, run_fast, scene_outcome};
use shopfloor_core::runtime::now_ms;
use shopfloor_core::scenario::{SinkDef, SinkKind};
use shopfloor_core::scene::Repeat;
use shopfloor_core::sinks::{EventSink, SinkMonitor, build_sink};
use shopfloor_core::{Scenario, SimRuntime, load_scenario};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "shopfloor", version, about = "Run shopfloor scenarios and scenes without the GUI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate a scenario file and list what it contains.
    Check { scenario: PathBuf },
    /// Run a scenario, optionally playing one of its scenes, and write events to its sinks.
    Run {
        scenario: PathBuf,
        /// Scene to play from the start. The run ends when the scene finishes or fails.
        #[arg(long)]
        scene: Option<String>,
        /// Stop after this many seconds of simulation time.
        #[arg(long, value_name = "SECS")]
        duration: Option<f64>,
        /// Run as fast as possible instead of in real time (events are still stamped as if the
        /// run happened in real time).
        #[arg(long)]
        fast: bool,
        /// Also write every event to this JSONL file (overwritten).
        #[arg(long, value_name = "PATH")]
        jsonl: Option<PathBuf>,
        /// Also stream every event over TCP on this address, e.g. 127.0.0.1:9000 (where consumers
        /// connect). Ignored with --fast.
        #[arg(long, value_name = "ADDR")]
        tcp: Option<String>,
    },
}

/// How a run ended.
enum RunEnd {
    Scene(Outcome),
    Interrupted,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let result = match Cli::parse().command {
        Command::Check { scenario } => check(&scenario),
        Command::Run {
            scenario,
            scene,
            duration,
            fast,
            jsonl,
            tcp,
        } => run(&scenario, scene, duration, fast, jsonl, tcp),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn check(path: &Path) -> anyhow::Result<ExitCode> {
    let scenario = load_scenario(path)?;
    println!("Scenario '{}' ({}): OK", scenario.name, path.display());
    println!(
        "Floor {} × {} m, tick {} ms, seed {}",
        scenario.floor.width_m, scenario.floor.height_m, scenario.sim.tick_ms, scenario.sim.seed
    );
    println!("Machines ({}):", scenario.machines.len());
    for machine in &scenario.machines {
        println!("  {:<18} {}", machine.id, machine.label);
    }
    println!("Mobile manipulators ({}):", scenario.mobile_manipulators.len());
    for mm in &scenario.mobile_manipulators {
        println!(
            "  {:<18} {} · actions: {}",
            mm.id,
            mm.label,
            list_or_none(&mm.actions)
        );
    }
    println!("Sensors ({}):", scenario.sensors.len());
    for sensor in &scenario.sensors {
        println!("  {:<18} {} [{}]", sensor.id, sensor.kind, sensor.unit);
    }
    println!("Actions ({}):", scenario.actions.len());
    for action in &scenario.actions {
        let mut rules = Vec::new();
        if action.requires_machine {
            rules.push("at a machine");
        }
        if !action.allowed_while_moving {
            rules.push("stopped");
        }
        println!("  {:<18} {} ({})", action.id, action.label, rules.join(", "));
    }
    println!("Scenes ({}):", scenario.scenes.len());
    for scene in &scenario.scenes {
        let repeat = match scene.repeat {
            Repeat::Once => "once".to_string(),
            Repeat::Times(times) => format!("{times}×"),
            Repeat::Forever => "loop".to_string(),
        };
        let tracks: Vec<String> = scene
            .tracks
            .iter()
            .map(|track| format!("{} ({} steps)", track.name, track.steps.len()))
            .collect();
        println!(
            "  {:<18} {} · {repeat} · {}",
            scene.id,
            scene.label,
            tracks.join(", ")
        );
    }
    let sinks: Vec<String> = scenario.output.sinks.iter().map(describe_sink).collect();
    println!("Sinks: {}", list_or_none(&sinks));
    Ok(ExitCode::SUCCESS)
}

fn run(
    path: &Path,
    scene: Option<String>,
    duration: Option<f64>,
    fast: bool,
    jsonl: Option<PathBuf>,
    tcp: Option<String>,
) -> anyhow::Result<ExitCode> {
    let mut scenario = load_scenario(path)?;
    if let Some(bind) = tcp {
        scenario.output.sinks.push(SinkDef {
            name: "cli-tcp".to_string(),
            kind: SinkKind::Tcp { bind },
        });
    }
    if let Some(path) = jsonl {
        scenario.output.sinks.push(SinkDef {
            name: "cli-jsonl".to_string(),
            kind: SinkKind::Jsonl {
                path,
                append: false,
            },
        });
    }
    if scenario.output.sinks.is_empty() {
        tracing::warn!("no [output.sinks] in the scenario and no --jsonl: events go nowhere");
    }
    if let Some(id) = &scene
        && scenario.scene_index(id).is_none()
    {
        let known: Vec<String> = scenario.scenes.iter().map(|s| s.id.clone()).collect();
        bail!("unknown scene '{id}' (known: {})", list_or_none(&known));
    }
    let max_sim_ms = match duration {
        Some(seconds) if !(seconds.is_finite() && seconds > 0.0) => {
            bail!("--duration must be a positive number of seconds")
        }
        Some(seconds) => Some((seconds * 1000.0).round() as u64),
        None => None,
    };
    let scenario = Arc::new(scenario);

    let end = if fast {
        // Streaming minutes of data to a consumer in a fraction of a second isn't useful, so TCP
        // and MQTT sinks only run in real time.
        let monitor = SinkMonitor::default();
        let mut sinks: Vec<Box<dyn EventSink>> = Vec::new();
        for def in &scenario.output.sinks {
            if def.kind.is_live() {
                tracing::info!("--fast: skipping live sink '{}'", def.name);
                continue;
            }
            sinks.push(build_sink(def, &monitor)?);
        }
        let run = FastRun {
            scene: scene.clone(),
            max_sim_ms,
            start_ts: now_ms(),
        };
        let summary = run_fast(Arc::clone(&scenario), &run, &mut sinks)?;
        tracing::info!(
            "{} events over {:.1} s of simulation time",
            summary.events,
            summary.sim_ms as f64 / 1000.0
        );
        RunEnd::Scene(summary.outcome)
    } else {
        tokio::runtime::Runtime::new()
            .context("failed to start the async runtime")?
            .block_on(run_realtime(scenario, scene.clone(), max_sim_ms))?
    };

    let scene_name = scene.as_deref().unwrap_or("-");
    Ok(match end {
        RunEnd::Scene(Outcome::Finished) => {
            tracing::info!("scene '{scene_name}' finished");
            ExitCode::SUCCESS
        }
        RunEnd::Scene(Outcome::Failed(error)) => {
            tracing::error!("scene '{scene_name}' failed: {error}");
            ExitCode::from(1)
        }
        RunEnd::Scene(Outcome::TimeLimit) => {
            tracing::info!("duration reached");
            ExitCode::SUCCESS
        }
        RunEnd::Interrupted => {
            tracing::info!("interrupted");
            ExitCode::SUCCESS
        }
    })
}

/// Real time: the same runtime the GUI uses. Ends on scene end, `--duration`, or Ctrl+C.
async fn run_realtime(
    scenario: Arc<Scenario>,
    scene: Option<String>,
    max_sim_ms: Option<u64>,
) -> anyhow::Result<RunEnd> {
    let runtime = SimRuntime::spawn(scenario)?;
    // Subscribe before playing so the scene's first events are seen.
    let mut events = runtime.subscribe();
    if let Some(id) = &scene {
        runtime.control(Control::PlayScene(id.clone())).await?;
        tracing::info!("playing scene '{id}'");
    }

    let interrupted = tokio::signal::ctrl_c();
    let limit = async {
        match max_sim_ms {
            Some(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(interrupted, limit);

    let end = loop {
        tokio::select! {
            _ = &mut interrupted => break RunEnd::Interrupted,
            _ = &mut limit => break RunEnd::Scene(Outcome::TimeLimit),
            received = events.recv() => match received {
                Ok(batch) => {
                    if scene.is_some()
                        && let Some(outcome) = scene_outcome(&batch)
                    {
                        break RunEnd::Scene(outcome);
                    }
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => bail!("the simulation stopped unexpectedly"),
            },
        }
    };
    runtime.shutdown().await;
    Ok(end)
}

fn describe_sink(sink: &SinkDef) -> String {
    match &sink.kind {
        SinkKind::Stdout => format!("{} (stdout)", sink.name),
        SinkKind::Jsonl { path, .. } => format!("{} (jsonl -> {})", sink.name, path.display()),
        SinkKind::Tcp { bind } => format!("{} (tcp, listening on {bind})", sink.name),
        SinkKind::Mqtt { broker, topics, .. } => {
            let topics: Vec<&str> = topics.iter().map(|topic| topic.topic.as_str()).collect();
            format!("{} (mqtt -> {broker}: {})", sink.name, topics.join(", "))
        }
    }
}

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}
