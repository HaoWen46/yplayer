use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::anyhow;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::player::mpv::{CommandTimeout, MpvApi, MpvEvent, MpvSpawner};
use crate::player::queue::Queue;
use crate::protocol::{ContextRef, PlayState, PlayerState};
use crate::types::LoopMode;

/// Quit mpv after this long paused or stopped.
const IDLE_SHUTDOWN: Duration = Duration::from_secs(600);
/// `prev` restarts the current track instead when past this many seconds.
const PREV_RESTARTS_AFTER: f64 = 3.0;
/// Consecutive command timeouts after which mpv is killed.
const MAX_TIMEOUTS: u32 = 2;

pub trait TrackResolver {
    fn playable_path(&self, track_id: &str) -> Option<String>;

    /// Gain to play `track_id` with, in dB (mpv's per-file `volume-gain`).
    fn gain_db(&self, _track_id: &str) -> f64 {
        0.0
    }
}

#[derive(Debug)]
pub enum EngineError {
    Unplayable(String),
    Mpv(anyhow::Error),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Unplayable(id) => write!(f, "track {id} is not playable"),
            EngineError::Mpv(e) => write!(f, "mpv: {e}"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<anyhow::Error> for EngineError {
    fn from(e: anyhow::Error) -> Self {
        EngineError::Mpv(e)
    }
}

#[derive(Clone, Copy)]
enum Step {
    Auto,
    Next,
    Prev,
}

/// Owns the one mpv process and the play queue. mpv's playlist is kept at
/// `[current, preloaded next]` so track changes are gapless.
pub struct Engine<S: MpvSpawner> {
    spawner: S,
    events: mpsc::UnboundedSender<(u64, MpvEvent)>,
    mpv: Option<S::Mpv>,
    /// Bumped per spawn; events tagged with another generation are stale.
    generation: u64,
    queue: Queue,
    state: PlayState,
    volume: f64,
    position: f64,
    at_ms: i64,
    duration: Option<f64>,
    /// Path loaded for the current track; `resume` reloads it after an idle quit.
    current_path: Option<String>,
    /// `volume-gain` the current track plays with; `resume` reloads with it.
    current_gain: f64,
    /// `(track_id, path, gain)` appended after the current entry.
    preloaded: Option<(String, String, f64)>,
    /// A `loadfile replace` has not reached `FileLoaded` yet: the preload
    /// waits for it, and a stale `idle-active=true` is not an end of playback.
    awaiting_load: bool,
    mpv_paused: bool,
    idle_deadline: Option<Instant>,
    resume_point: Option<(String, f64)>,
    /// Commands in a row that got no reply from the current process.
    timeouts: u32,
    /// The last spawn attempt failed.
    spawn_failed: bool,
    /// For the user: mpv died or hung.
    warning: Option<String>,
}

impl<S: MpvSpawner> Engine<S> {
    pub fn new(
        spawner: S,
        events: mpsc::UnboundedSender<(u64, MpvEvent)>,
        volume: f64,
        loop_mode: LoopMode,
    ) -> Self {
        let mut queue = Queue::new();
        queue.set_loop(loop_mode);
        Engine {
            spawner,
            events,
            mpv: None,
            generation: 0,
            queue,
            state: PlayState::Stopped,
            volume,
            position: 0.0,
            at_ms: now_ms(),
            duration: None,
            current_path: None,
            current_gain: 0.0,
            preloaded: None,
            awaiting_load: false,
            mpv_paused: false,
            idle_deadline: None,
            resume_point: None,
            timeouts: 0,
            spawn_failed: false,
            warning: None,
        }
    }

    pub async fn play(
        &mut self,
        context: ContextRef,
        order: Vec<String>,
        start_id: &str,
        r: &impl TrackResolver,
    ) -> Result<(), EngineError> {
        let path = r
            .playable_path(start_id)
            .ok_or_else(|| EngineError::Unplayable(start_id.to_string()))?;
        let gain = r.gain_db(start_id);
        let saved = self.queue.clone();
        self.queue.start(context, order, start_id);
        let result = self.load(path, 0.0, gain).await;
        if result.is_err() {
            self.queue = saved;
        }
        result
    }

    pub async fn pause(&mut self) -> Result<(), EngineError> {
        if self.state != PlayState::Playing {
            return Ok(());
        }
        self.set_pause(true).await?;
        self.state = PlayState::Paused;
        self.arm_idle();
        Ok(())
    }

    pub async fn resume(&mut self) -> Result<(), EngineError> {
        match self.state {
            PlayState::Playing => Ok(()),
            PlayState::Paused if self.mpv_alive() => {
                self.set_pause(false).await?;
                self.state = PlayState::Playing;
                self.idle_deadline = None;
                Ok(())
            }
            state => {
                // mpv was quit while idle, died, or playback stopped: reload.
                let (Some(id), Some(path)) = (
                    self.queue.current().map(str::to_string),
                    self.current_path.clone(),
                ) else {
                    return self.check_player().await;
                };
                let start = match &self.resume_point {
                    Some((rid, pos)) if *rid == id => *pos,
                    _ if state == PlayState::Paused => self.position,
                    _ => 0.0,
                };
                self.load(path, start, self.current_gain).await
            }
        }
    }

    pub async fn toggle(&mut self) -> Result<(), EngineError> {
        if self.state == PlayState::Playing {
            self.pause().await
        } else {
            self.resume().await
        }
    }

    pub async fn stop(&mut self) -> Result<(), EngineError> {
        if self.mpv_alive() {
            self.cmd(vec![json!("stop")]).await?;
        }
        self.mark_stopped();
        self.resume_point = None;
        Ok(())
    }

    pub async fn next(&mut self, r: &impl TrackResolver) -> Result<(), EngineError> {
        if self.step_and_load(Step::Next, r).await? {
            return Ok(());
        }
        self.check_player().await?;
        self.stop().await
    }

    pub async fn prev(&mut self, r: &impl TrackResolver) -> Result<(), EngineError> {
        if self.current_position() > PREV_RESTARTS_AFTER {
            return self.seek(0.0).await;
        }
        if self.step_and_load(Step::Prev, r).await? {
            return Ok(());
        }
        self.check_player().await?;
        self.seek(0.0).await
    }

    /// The download of `id` failed: if it is the current track, move on to
    /// the next one (or stop) instead of playing a failed track.
    pub async fn track_failed(
        &mut self,
        id: &str,
        r: &impl TrackResolver,
    ) -> Result<(), EngineError> {
        if self.queue.current() != Some(id) || self.state == PlayState::Stopped {
            return Ok(());
        }
        self.next(r).await
    }

    /// `position` must be finite and ≥ 0 (the service checks).
    pub async fn seek(&mut self, position: f64) -> Result<(), EngineError> {
        if self.mpv_alive() {
            self.cmd(vec![json!("seek"), json!(position), json!("absolute")])
                .await?;
        } else {
            self.position = position;
            if let Some((_, pos)) = &mut self.resume_point {
                *pos = position;
            }
        }
        Ok(())
    }

    /// `v` must be finite (the service checks); it is clamped to 0..=100
    /// and kept only once mpv accepted it.
    pub async fn set_volume(&mut self, v: f64) -> Result<(), EngineError> {
        let v = v.clamp(0.0, 100.0);
        if self.mpv_alive() {
            self.cmd(vec![json!("set_property"), json!("volume"), json!(v)])
                .await?;
        }
        self.volume = v;
        Ok(())
    }

    pub async fn set_loop(
        &mut self,
        mode: LoopMode,
        r: &impl TrackResolver,
    ) -> Result<(), EngineError> {
        self.queue.set_loop(mode);
        if self.mpv_alive() {
            self.cmd(loop_file(mode)).await?;
            self.refresh_preload(r).await?;
        }
        Ok(())
    }

    pub async fn play_next(&mut self, id: &str, r: &impl TrackResolver) -> Result<(), EngineError> {
        self.queue.play_next(id);
        self.refresh_preload(r).await
    }

    /// If `id` is current, playback moves on to the next track (or stops).
    pub async fn remove_track(
        &mut self,
        id: &str,
        r: &impl TrackResolver,
    ) -> Result<(), EngineError> {
        let was_current = self.queue.current() == Some(id);
        self.queue.remove(id);
        if was_current && self.state != PlayState::Stopped {
            return self.next(r).await;
        }
        self.refresh_preload(r).await
    }

    pub async fn replace_order(
        &mut self,
        order: Vec<String>,
        r: &impl TrackResolver,
    ) -> Result<(), EngineError> {
        self.queue.replace_order(order);
        self.refresh_preload(r).await
    }

    /// Returns true when the player state changed.
    pub async fn on_mpv_event(
        &mut self,
        generation: u64,
        ev: MpvEvent,
        r: &impl TrackResolver,
    ) -> bool {
        if generation != self.generation || self.mpv.is_none() {
            return false;
        }
        match ev {
            MpvEvent::PropertyChange { name, data } => match name.as_str() {
                "pause" => {
                    self.mpv_paused = data.as_bool().unwrap_or(false);
                    self.sample_position().await;
                    true
                }
                "duration" => {
                    let duration = data.as_f64();
                    let changed = duration != self.duration;
                    self.duration = duration;
                    changed
                }
                "volume" => match data.as_f64() {
                    Some(v) if v != self.volume => {
                        self.volume = v;
                        true
                    }
                    _ => false,
                },
                "idle-active" => {
                    if data.as_bool() != Some(true)
                        || self.state != PlayState::Playing
                        || self.awaiting_load
                    {
                        return false;
                    }
                    // The playlist ran out: nothing resolvable was preloaded.
                    self.preloaded = None;
                    self.advance_or_stop(Step::Auto, r).await;
                    true
                }
                "path" => match self.preloaded.take() {
                    Some((_, path, gain)) if data.as_str() == Some(path.as_str()) => {
                        // mpv moved on to the preloaded entry.
                        self.queue.advance_auto();
                        self.current_path = Some(path);
                        self.current_gain = gain;
                        self.position = 0.0;
                        self.duration = None;
                        self.at_ms = now_ms();
                        let _ = self.preload(r).await;
                        true
                    }
                    other => {
                        self.preloaded = other;
                        false
                    }
                },
                _ => false,
            },
            MpvEvent::EndFile { reason } => {
                if reason != "error" || self.state == PlayState::Stopped {
                    return false;
                }
                self.advance_or_stop(Step::Next, r).await;
                true
            }
            MpvEvent::FileLoaded => {
                if self.awaiting_load {
                    self.awaiting_load = false;
                    let _ = self.preload(r).await;
                }
                false
            }
            MpvEvent::PlaybackRestart | MpvEvent::Seek => {
                self.sample_position().await;
                true
            }
            MpvEvent::Exited => {
                // Quits take `mpv` first, so this exit was unexpected:
                // `resume` continues where it stopped.
                let position = self.current_position();
                self.mpv = None;
                self.resume_point = self.queue.current().map(|id| (id.to_string(), position));
                self.mark_stopped();
                self.warning = Some("The player quit unexpectedly".into());
                true
            }
        }
    }

    /// A track's gain changed (a new measurement): re-preload the next track
    /// if its gain differs. The playing track keeps its gain.
    pub async fn refresh_gains(&mut self, r: &impl TrackResolver) -> Result<(), EngineError> {
        self.refresh_preload(r).await
    }

    /// Loudness leveling was switched: set the playing track's `volume-gain`
    /// live and re-preload the next track with its new gain.
    pub async fn apply_leveling(&mut self, r: &impl TrackResolver) -> Result<(), EngineError> {
        let Some(id) = self.queue.current() else {
            return Ok(());
        };
        let gain = r.gain_db(id);
        self.current_gain = gain;
        if self.mpv_alive() && self.state != PlayState::Stopped {
            self.cmd(vec![
                json!("set_property"),
                json!("volume-gain"),
                json!(round_gain(gain)),
            ])
            .await?;
        }
        self.refresh_preload(r).await
    }

    /// The track appended after the current entry.
    pub fn preloaded_id(&self) -> Option<&str> {
        self.preloaded.as_ref().map(|(id, _, _)| id.as_str())
    }

    pub fn idle_deadline(&self) -> Option<Instant> {
        self.idle_deadline
    }

    /// A warning for the user, set when mpv died or hung.
    pub fn take_warning(&mut self) -> Option<String> {
        self.warning.take()
    }

    /// Quit the idle mpv, remembering where to resume.
    pub async fn on_idle_deadline(&mut self) {
        self.idle_deadline = None;
        if self.state == PlayState::Playing {
            return;
        }
        let Some(mut mpv) = self.mpv.take() else {
            return;
        };
        self.resume_point = self
            .queue
            .current()
            .map(|id| (id.to_string(), self.position));
        mpv.quit().await;
        self.preloaded = None;
        self.awaiting_load = false;
    }

    pub fn state(&self) -> PlayerState {
        PlayerState {
            state: self.state,
            track_id: self.queue.current().map(str::to_string),
            context: self.queue.context().cloned(),
            position: self.position,
            at_ms: self.at_ms,
            duration: self.duration,
            volume: self.volume,
            loop_mode: self.queue.loop_mode(),
        }
    }

    fn mpv_alive(&self) -> bool {
        self.mpv.as_ref().is_some_and(|m| m.alive())
    }

    async fn cmd(&mut self, args: Vec<Value>) -> Result<Value, EngineError> {
        let mpv = self
            .mpv
            .as_mut()
            .ok_or_else(|| EngineError::Mpv(anyhow!("mpv is not running")))?;
        let result = mpv.command(args).await;
        match &result {
            Err(e) if e.is::<CommandTimeout>() => {
                self.timeouts += 1;
                if self.timeouts >= MAX_TIMEOUTS {
                    self.kill_unresponsive().await;
                }
            }
            _ => self.timeouts = 0,
        }
        Ok(result?)
    }

    /// mpv stopped answering: SIGKILL it and stop; the next command that
    /// plays spawns a new one.
    async fn kill_unresponsive(&mut self) {
        if let Some(mut mpv) = self.mpv.take() {
            mpv.kill().await;
        }
        self.mark_stopped();
        self.warning = Some("The player stopped responding and was restarted".into());
    }

    /// When mpv is not running and the last spawn failed, try again, so a
    /// command with nothing to load reports `player_unavailable` instead of
    /// succeeding silently.
    async fn check_player(&mut self) -> Result<(), EngineError> {
        if !self.spawn_failed || self.mpv_alive() {
            return Ok(());
        }
        self.ensure_mpv().await?;
        self.arm_idle();
        Ok(())
    }

    async fn ensure_mpv(&mut self) -> Result<(), EngineError> {
        if self.mpv_alive() {
            return Ok(());
        }
        self.mpv = None;
        self.generation += 1;
        self.timeouts = 0;
        let spawned = self
            .spawner
            .spawn(self.volume, self.generation, self.events.clone())
            .await;
        self.spawn_failed = spawned.is_err();
        self.mpv = Some(spawned?);
        self.mpv_paused = false;
        self.preloaded = None;
        if self.queue.loop_mode() == LoopMode::Single {
            self.cmd(loop_file(LoopMode::Single)).await?;
        }
        Ok(())
    }

    /// `loadfile <path> replace -1 volume-gain=<gain>` (plus `start=<pos>`
    /// when resuming) and play.
    async fn load(&mut self, path: String, start: f64, gain: f64) -> Result<(), EngineError> {
        self.ensure_mpv().await?;
        let mut options = gain_option(gain);
        if start > 0.0 {
            options.push_str(&format!(",start={start}"));
        }
        self.cmd(vec![
            json!("loadfile"),
            json!(path.as_str()),
            json!("replace"),
            json!(-1),
            json!(options),
        ])
        .await?;
        if self.mpv_paused {
            self.set_pause(false).await?;
        }
        self.state = PlayState::Playing;
        self.current_path = Some(path);
        self.current_gain = gain;
        self.duration = None;
        self.preloaded = None;
        self.awaiting_load = true;
        self.position = start;
        self.at_ms = now_ms();
        self.idle_deadline = None;
        self.resume_point = None;
        Ok(())
    }

    /// Move the queue with `step` and load the track reached; `Ok(false)`
    /// when there is none. The queue only moves when the load succeeds.
    async fn step_and_load(
        &mut self,
        step: Step,
        r: &impl TrackResolver,
    ) -> Result<bool, EngineError> {
        let saved = self.queue.clone();
        let result = match self.step(step, r) {
            Some((path, gain)) => self.load(path, 0.0, gain).await.map(|()| true),
            None => Ok(false),
        };
        if !matches!(result, Ok(true)) {
            self.queue = saved;
        }
        result
    }

    async fn advance_or_stop(&mut self, step: Step, r: &impl TrackResolver) {
        if !matches!(self.step_and_load(step, r).await, Ok(true)) {
            self.mark_stopped();
        }
    }

    async fn set_pause(&mut self, paused: bool) -> Result<(), EngineError> {
        self.cmd(vec![json!("set_property"), json!("pause"), json!(paused)])
            .await?;
        self.mpv_paused = paused;
        Ok(())
    }

    /// Move the queue, skipping ids with no playable file; the path and gain
    /// of the track reached, or `None` when there is none.
    fn step(&mut self, step: Step, r: &impl TrackResolver) -> Option<(String, f64)> {
        let mut seen = HashSet::new();
        loop {
            let id = match step {
                Step::Auto => self.queue.advance_auto(),
                Step::Next => self.queue.next_manual(),
                Step::Prev => self.queue.prev_manual(),
            }?;
            if let Some(path) = r.playable_path(&id) {
                return Some((path, r.gain_db(&id)));
            }
            if !seen.insert(id) {
                return None;
            }
        }
    }

    fn preload_target(&self, r: &impl TrackResolver) -> Option<(String, String, f64)> {
        // `Single` repeats through mpv's `loop-file`.
        if self.queue.loop_mode() == LoopMode::Single {
            return None;
        }
        let id = self.queue.peek_next_auto()?;
        let path = r.playable_path(&id)?;
        let gain = r.gain_db(&id);
        Some((id, path, gain))
    }

    /// Make mpv's playlist `[current, next]`; the next entry carries its
    /// `volume-gain`.
    async fn preload(&mut self, r: &impl TrackResolver) -> Result<(), EngineError> {
        let target = self.preload_target(r);
        self.cmd(vec![json!("playlist-clear")]).await?;
        if let Some((_, path, gain)) = &target {
            self.cmd(vec![
                json!("loadfile"),
                json!(path),
                json!("append"),
                json!(-1),
                json!(gain_option(*gain)),
            ])
            .await?;
        }
        self.preloaded = target;
        Ok(())
    }

    /// Re-preload after the queue or a gain changed, if the next track (id,
    /// path or gain) differs.
    async fn refresh_preload(&mut self, r: &impl TrackResolver) -> Result<(), EngineError> {
        if !self.mpv_alive() || self.state == PlayState::Stopped || self.awaiting_load {
            return Ok(());
        }
        if self.preload_target(r) == self.preloaded {
            return Ok(());
        }
        self.preload(r).await
    }

    async fn sample_position(&mut self) {
        if let Ok(v) = self
            .cmd(vec![json!("get_property"), json!("time-pos")])
            .await
            && let Some(pos) = v.as_f64()
        {
            self.position = pos;
            self.at_ms = now_ms();
        }
    }

    fn current_position(&self) -> f64 {
        if self.state == PlayState::Playing {
            self.position + (now_ms() - self.at_ms) as f64 / 1000.0
        } else {
            self.position
        }
    }

    fn mark_stopped(&mut self) {
        self.state = PlayState::Stopped;
        self.position = 0.0;
        self.at_ms = now_ms();
        self.preloaded = None;
        self.awaiting_load = false;
        self.arm_idle();
    }

    fn arm_idle(&mut self) {
        self.idle_deadline = self.mpv.is_some().then(|| Instant::now() + IDLE_SHUTDOWN);
    }
}

fn loop_file(mode: LoopMode) -> Vec<Value> {
    let value = if mode == LoopMode::Single {
        "inf"
    } else {
        "no"
    };
    vec![json!("set_property"), json!("loop-file"), json!(value)]
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// `gain` to two decimals, never `-0`.
fn round_gain(gain: f64) -> f64 {
    let gain = (gain * 100.0).round() / 100.0;
    if gain == 0.0 { 0.0 } else { gain }
}

/// Per-file loadfile option: `volume-gain=<gain, 2 decimals>`.
fn gain_option(gain: f64) -> String {
    format!("volume-gain={:.2}", round_gain(gain))
}

/// Fake mpv for engine and service tests.
#[cfg(test)]
pub mod testing {
    use std::sync::{Arc, Mutex};

    use anyhow::{Result, bail};
    use serde_json::{Value, json};
    use tokio::sync::mpsc;

    use crate::player::mpv::{CommandTimeout, MpvApi, MpvEvent, MpvSpawner};

    /// Shared by the spawner and every fake process it made.
    #[derive(Debug, Default)]
    pub struct FakeLog {
        pub commands: Vec<Vec<Value>>,
        pub spawns: u64,
        pub quits: u64,
        pub kills: u64,
        pub generation: u64,
        pub alive: bool,
        /// Every command times out, as if mpv stopped answering.
        pub hung: bool,
        pub time_pos: f64,
        pub events: Option<mpsc::UnboundedSender<(u64, MpvEvent)>>,
    }

    /// Records every command; answers `get_property time-pos` from
    /// `time_pos` and everything else with `null` (or times out while
    /// `hung`). Emits nothing by itself.
    #[derive(Debug, Clone, Default)]
    pub struct FakeSpawner(pub Arc<Mutex<FakeLog>>);

    impl FakeSpawner {
        pub fn commands(&self) -> Vec<Vec<Value>> {
            self.0.lock().unwrap().commands.clone()
        }

        pub fn clear_commands(&self) {
            self.0.lock().unwrap().commands.clear();
        }

        pub fn spawns(&self) -> u64 {
            self.0.lock().unwrap().spawns
        }

        pub fn quits(&self) -> u64 {
            self.0.lock().unwrap().quits
        }

        pub fn kills(&self) -> u64 {
            self.0.lock().unwrap().kills
        }

        pub fn set_hung(&self, hung: bool) {
            self.0.lock().unwrap().hung = hung;
        }

        /// Generation of the latest spawn.
        pub fn generation(&self) -> u64 {
            self.0.lock().unwrap().generation
        }

        pub fn set_time_pos(&self, pos: f64) {
            self.0.lock().unwrap().time_pos = pos;
        }

        /// Send `ev` on the events channel as the latest process.
        pub fn emit(&self, ev: MpvEvent) {
            let log = self.0.lock().unwrap();
            if let Some(tx) = &log.events {
                let _ = tx.send((log.generation, ev));
            }
        }
    }

    pub struct FakeMpv {
        log: Arc<Mutex<FakeLog>>,
        generation: u64,
    }

    impl MpvApi for FakeMpv {
        async fn command(&mut self, args: Vec<Value>) -> Result<Value> {
            if !self.alive() {
                bail!("fake mpv is not running");
            }
            let mut log = self.log.lock().unwrap();
            if log.hung {
                let name = args.first().and_then(Value::as_str).unwrap_or("");
                return Err(CommandTimeout(name.to_string()).into());
            }
            let reply = if args == [json!("get_property"), json!("time-pos")] {
                json!(log.time_pos)
            } else {
                Value::Null
            };
            log.commands.push(args);
            Ok(reply)
        }

        async fn quit(&mut self) {
            let mut log = self.log.lock().unwrap();
            log.quits += 1;
            if log.generation == self.generation {
                log.alive = false;
            }
        }

        async fn kill(&mut self) {
            let mut log = self.log.lock().unwrap();
            log.kills += 1;
            if log.generation == self.generation {
                log.alive = false;
            }
        }

        fn alive(&self) -> bool {
            let log = self.log.lock().unwrap();
            log.alive && log.generation == self.generation
        }
    }

    impl MpvSpawner for FakeSpawner {
        type Mpv = FakeMpv;

        async fn spawn(
            &self,
            _volume: f64,
            generation: u64,
            events: mpsc::UnboundedSender<(u64, MpvEvent)>,
        ) -> Result<FakeMpv> {
            let mut log = self.0.lock().unwrap();
            log.spawns += 1;
            log.generation = generation;
            log.alive = true;
            log.events = Some(events);
            Ok(FakeMpv {
                log: self.0.clone(),
                generation,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::{Value, json};
    use tokio::sync::mpsc;
    use tokio::time::Instant;

    use super::testing::FakeSpawner;
    use super::*;
    use crate::player::mpv::{MpvOptions, MpvProcess, MpvSpawner, ProcessSpawner};

    struct Paths(HashMap<String, String>);

    impl TrackResolver for Paths {
        fn playable_path(&self, track_id: &str) -> Option<String> {
            self.0.get(track_id).cloned()
        }
    }

    fn p(id: &str) -> String {
        format!("/m/{id}.opus")
    }

    fn paths(resolvable: &[&str]) -> Paths {
        Paths(
            resolvable
                .iter()
                .map(|id| (id.to_string(), p(id)))
                .collect(),
        )
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn cmd(args: &[Value]) -> Vec<Value> {
        args.to_vec()
    }

    /// `loadfile` at gain 0 (resolvers without gains).
    fn loadfile(path: &str, mode: &str) -> Vec<Value> {
        loadfile_opts(path, mode, "volume-gain=0.00")
    }

    fn loadfile_opts(path: &str, mode: &str, options: &str) -> Vec<Value> {
        cmd(&[
            json!("loadfile"),
            json!(path),
            json!(mode),
            json!(-1),
            json!(options),
        ])
    }

    /// `Paths` plus per-track gains (0 when absent).
    struct Gained<'a>(&'a Paths, HashMap<&'static str, f64>);

    impl TrackResolver for Gained<'_> {
        fn playable_path(&self, track_id: &str) -> Option<String> {
            self.0.playable_path(track_id)
        }

        fn gain_db(&self, track_id: &str) -> f64 {
            self.1.get(track_id).copied().unwrap_or(0.0)
        }
    }

    fn prop(name: &str, data: Value) -> MpvEvent {
        MpvEvent::PropertyChange {
            name: name.into(),
            data,
        }
    }

    fn engine(
        fake: &FakeSpawner,
        mode: LoopMode,
    ) -> (
        Engine<FakeSpawner>,
        mpsc::UnboundedReceiver<(u64, MpvEvent)>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Engine::new(fake.clone(), tx, 70.0, mode), rx)
    }

    /// Engine playing `start` of `order` with its file loaded and commands cleared.
    async fn playing(
        fake: &FakeSpawner,
        mode: LoopMode,
        order: &[&str],
        start: &str,
        r: &Paths,
    ) -> Engine<FakeSpawner> {
        let (mut e, _rx) = engine(fake, mode);
        e.play(ContextRef::Album(1), ids(order), start, r)
            .await
            .unwrap();
        e.on_mpv_event(fake.generation(), MpvEvent::FileLoaded, r)
            .await;
        fake.clear_commands();
        e
    }

    fn track(e: &Engine<FakeSpawner>) -> Option<String> {
        e.state().track_id
    }

    #[tokio::test]
    async fn play_loads_then_preloads_next_after_file_loaded() {
        let fake = FakeSpawner::default();
        let r = paths(&["a", "b", "c"]);
        let (mut e, _rx) = engine(&fake, LoopMode::None);
        e.play(ContextRef::Album(1), ids(&["a", "b", "c"]), "a", &r)
            .await
            .unwrap();
        assert_eq!(fake.spawns(), 1);
        assert_eq!(fake.commands(), vec![loadfile(&p("a"), "replace")]);
        let st = e.state();
        assert_eq!(st.state, PlayState::Playing);
        assert_eq!(st.track_id.as_deref(), Some("a"));
        assert_eq!(st.context, Some(ContextRef::Album(1)));

        e.on_mpv_event(fake.generation(), MpvEvent::FileLoaded, &r)
            .await;
        assert_eq!(
            fake.commands(),
            vec![
                loadfile(&p("a"), "replace"),
                cmd(&[json!("playlist-clear")]),
                loadfile(&p("b"), "append"),
            ]
        );
    }

    #[tokio::test]
    async fn path_change_to_preloaded_next_advances_and_preloads_following() {
        let fake = FakeSpawner::default();
        let r = paths(&["a", "b", "c"]);
        let mut e = playing(&fake, LoopMode::None, &["a", "b", "c"], "a", &r).await;
        let g = fake.generation();

        // The current file's own path is not a transition.
        assert!(!e.on_mpv_event(g, prop("path", json!(p("a"))), &r).await);
        assert_eq!(track(&e).as_deref(), Some("a"));
        e.on_mpv_event(g, prop("duration", json!(120.0)), &r).await;
        assert_eq!(e.state().duration, Some(120.0));

        assert!(e.on_mpv_event(g, prop("path", json!(p("b"))), &r).await);
        assert_eq!(track(&e).as_deref(), Some("b"));
        // The previous track's length must not linger until mpv reports b's.
        assert_eq!(e.state().duration, None);
        assert_eq!(
            fake.commands(),
            vec![cmd(&[json!("playlist-clear")]), loadfile(&p("c"), "append")]
        );

        fake.clear_commands();
        assert!(e.on_mpv_event(g, prop("path", json!(p("c"))), &r).await);
        assert_eq!(track(&e).as_deref(), Some("c"));
        // Last track in `None` mode: nothing to append.
        assert_eq!(fake.commands(), vec![cmd(&[json!("playlist-clear")])]);
        assert_eq!(fake.spawns(), 1);
    }

    #[tokio::test]
    async fn single_mode_sets_loop_file_inf() {
        let fake = FakeSpawner::default();
        let r = paths(&["a", "b"]);
        let (mut e, _rx) = engine(&fake, LoopMode::Single);
        e.play(ContextRef::Album(1), ids(&["a", "b"]), "a", &r)
            .await
            .unwrap();
        e.on_mpv_event(fake.generation(), MpvEvent::FileLoaded, &r)
            .await;
        assert_eq!(
            fake.commands(),
            vec![
                cmd(&[json!("set_property"), json!("loop-file"), json!("inf")]),
                loadfile(&p("a"), "replace"),
                cmd(&[json!("playlist-clear")]),
            ]
        );

        fake.clear_commands();
        e.set_loop(LoopMode::None, &r).await.unwrap();
        assert_eq!(
            fake.commands(),
            vec![
                cmd(&[json!("set_property"), json!("loop-file"), json!("no")]),
                cmd(&[json!("playlist-clear")]),
                loadfile(&p("b"), "append"),
            ]
        );
        assert_eq!(e.state().loop_mode, LoopMode::None);
    }

    #[tokio::test]
    async fn none_mode_last_track_idle_stops() {
        let fake = FakeSpawner::default();
        let r = paths(&["a", "b"]);
        let mut e = playing(&fake, LoopMode::None, &["a", "b"], "b", &r).await;
        let g = fake.generation();
        assert!(
            !e.on_mpv_event(g, prop("idle-active", json!(false)), &r)
                .await
        );
        assert_eq!(e.state().state, PlayState::Playing);

        assert!(
            e.on_mpv_event(g, prop("idle-active", json!(true)), &r)
                .await
        );
        let st = e.state();
        assert_eq!(st.state, PlayState::Stopped);
        assert_eq!(st.track_id.as_deref(), Some("b"));
        assert!(fake.commands().is_empty());
        assert!(e.idle_deadline().is_some());
    }

    #[tokio::test]
    async fn unresolvable_next_is_skipped() {
        let r = paths(&["a", "c"]);
        let fake = FakeSpawner::default();
        let (mut e, _rx) = engine(&fake, LoopMode::None);
        e.play(ContextRef::Album(1), ids(&["a", "b", "c"]), "a", &r)
            .await
            .unwrap();
        fake.clear_commands();
        // `b` is unresolvable, so nothing is preloaded after `a`.
        e.on_mpv_event(fake.generation(), MpvEvent::FileLoaded, &r)
            .await;
        assert_eq!(fake.commands(), vec![cmd(&[json!("playlist-clear")])]);

        // Natural end of `a` continues with `c`.
        fake.clear_commands();
        assert!(
            e.on_mpv_event(fake.generation(), prop("idle-active", json!(true)), &r)
                .await
        );
        assert_eq!(fake.commands(), vec![loadfile(&p("c"), "replace")]);
        assert_eq!(track(&e).as_deref(), Some("c"));
        assert_eq!(e.state().state, PlayState::Playing);

        // Manual next skips it too.
        let fake = FakeSpawner::default();
        let mut e = playing(&fake, LoopMode::None, &["a", "b", "c"], "a", &r).await;
        e.next(&r).await.unwrap();
        assert_eq!(fake.commands(), vec![loadfile(&p("c"), "replace")]);
        assert_eq!(track(&e).as_deref(), Some("c"));
    }

    #[tokio::test]
    async fn prev_within_3s_goes_back_and_after_3s_restarts() {
        let fake = FakeSpawner::default();
        let r = paths(&["a", "b", "c"]);
        let mut e = playing(&fake, LoopMode::None, &["a", "b", "c"], "b", &r).await;
        let g = fake.generation();

        fake.set_time_pos(1.0);
        e.on_mpv_event(g, MpvEvent::PlaybackRestart, &r).await;
        fake.clear_commands();
        e.prev(&r).await.unwrap();
        assert_eq!(fake.commands(), vec![loadfile(&p("a"), "replace")]);
        assert_eq!(track(&e).as_deref(), Some("a"));

        fake.set_time_pos(5.0);
        e.on_mpv_event(g, MpvEvent::PlaybackRestart, &r).await;
        fake.clear_commands();
        e.prev(&r).await.unwrap();
        assert_eq!(
            fake.commands(),
            vec![cmd(&[json!("seek"), json!(0.0), json!("absolute")])]
        );
        assert_eq!(track(&e).as_deref(), Some("a"));
    }

    #[tokio::test]
    async fn remove_current_track_advances() {
        let fake = FakeSpawner::default();
        let r = paths(&["a", "b", "c"]);
        let mut e = playing(&fake, LoopMode::None, &["a", "b", "c"], "a", &r).await;
        e.remove_track("a", &r).await.unwrap();
        assert_eq!(fake.commands(), vec![loadfile(&p("b"), "replace")]);
        assert_eq!(track(&e).as_deref(), Some("b"));
        assert_eq!(e.state().state, PlayState::Playing);
    }

    #[tokio::test]
    async fn end_file_error_skips_to_next() {
        let fake = FakeSpawner::default();
        let r = paths(&["a", "b", "c"]);
        let mut e = playing(&fake, LoopMode::None, &["a", "b", "c"], "a", &r).await;
        let ev = MpvEvent::EndFile {
            reason: "error".into(),
        };
        assert!(e.on_mpv_event(fake.generation(), ev, &r).await);
        assert_eq!(fake.commands(), vec![loadfile(&p("b"), "replace")]);
        assert_eq!(track(&e).as_deref(), Some("b"));

        // Other end reasons are not skips.
        fake.clear_commands();
        let ev = MpvEvent::EndFile {
            reason: "eof".into(),
        };
        assert!(!e.on_mpv_event(fake.generation(), ev, &r).await);
        assert!(fake.commands().is_empty());
    }

    #[tokio::test]
    async fn events_from_an_old_generation_are_ignored() {
        let fake = FakeSpawner::default();
        let r = paths(&["a", "b"]);
        let mut e = playing(&fake, LoopMode::None, &["a", "b"], "a", &r).await;
        let old = fake.generation();
        assert!(e.on_mpv_event(old, MpvEvent::Exited, &r).await);
        e.play(ContextRef::Album(1), ids(&["a", "b"]), "a", &r)
            .await
            .unwrap();
        assert_eq!(fake.generation(), old + 1);
        fake.clear_commands();

        assert!(!e.on_mpv_event(old, MpvEvent::FileLoaded, &r).await);
        assert!(
            !e.on_mpv_event(old, prop("idle-active", json!(true)), &r)
                .await
        );
        assert!(!e.on_mpv_event(old, MpvEvent::Exited, &r).await);
        assert!(fake.commands().is_empty());
        assert_eq!(e.state().state, PlayState::Playing);
    }

    #[tokio::test]
    async fn time_pos_is_only_queried_on_restart_seek_and_pause_change() {
        let fake = FakeSpawner::default();
        let r = paths(&["a", "b"]);
        let mut e = playing(&fake, LoopMode::None, &["a", "b"], "a", &r).await;
        let g = fake.generation();
        let queries = |fake: &FakeSpawner| {
            fake.commands()
                .iter()
                .filter(|c| **c == [json!("get_property"), json!("time-pos")])
                .count()
        };

        for ev in [
            prop("duration", json!(4.0)),
            prop("volume", json!(70.0)),
            prop("idle-active", json!(false)),
            prop("path", json!(p("a"))),
            prop("playlist-pos", json!(0)),
        ] {
            e.on_mpv_event(g, ev, &r).await;
        }
        assert_eq!(queries(&fake), 0);
        assert_eq!(e.state().duration, Some(4.0));

        fake.set_time_pos(1.5);
        assert!(e.on_mpv_event(g, MpvEvent::PlaybackRestart, &r).await);
        assert_eq!(queries(&fake), 1);
        assert_eq!(e.state().position, 1.5);

        fake.set_time_pos(2.0);
        assert!(e.on_mpv_event(g, MpvEvent::Seek, &r).await);
        assert_eq!(queries(&fake), 2);

        fake.set_time_pos(2.5);
        assert!(e.on_mpv_event(g, prop("pause", json!(true)), &r).await);
        assert_eq!(queries(&fake), 3);
        assert_eq!(e.state().position, 2.5);

        assert!(
            fake.commands()
                .iter()
                .all(|c| !c.contains(&json!("observe_property")))
        );
    }

    #[tokio::test]
    async fn idle_deadline_armed_on_pause_and_cleared_on_resume() {
        let fake = FakeSpawner::default();
        let r = paths(&["a"]);
        let mut e = playing(&fake, LoopMode::None, &["a"], "a", &r).await;
        assert!(e.idle_deadline().is_none());

        e.pause().await.unwrap();
        assert_eq!(e.state().state, PlayState::Paused);
        let left = e.idle_deadline().unwrap() - Instant::now();
        assert!(left > Duration::from_secs(599) && left <= Duration::from_secs(600));

        e.resume().await.unwrap();
        assert_eq!(e.state().state, PlayState::Playing);
        assert!(e.idle_deadline().is_none());
        assert_eq!(
            fake.commands(),
            vec![
                cmd(&[json!("set_property"), json!("pause"), json!(true)]),
                cmd(&[json!("set_property"), json!("pause"), json!(false)]),
            ]
        );
    }

    #[tokio::test]
    async fn idle_deadline_quits_and_resume_respawns_at_position() {
        let fake = FakeSpawner::default();
        let r = paths(&["a"]);
        let mut e = playing(&fake, LoopMode::None, &["a"], "a", &r).await;
        fake.set_time_pos(42.5);
        e.pause().await.unwrap();
        e.on_mpv_event(fake.generation(), prop("pause", json!(true)), &r)
            .await;

        e.on_idle_deadline().await;
        assert_eq!(fake.quits(), 1);
        assert!(e.idle_deadline().is_none());
        let st = e.state();
        assert_eq!(st.state, PlayState::Paused);
        assert_eq!(st.track_id.as_deref(), Some("a"));
        assert_eq!(st.position, 42.5);

        fake.clear_commands();
        e.resume().await.unwrap();
        assert_eq!(fake.spawns(), 2);
        assert_eq!(
            fake.commands(),
            vec![loadfile_opts(
                &p("a"),
                "replace",
                "volume-gain=0.00,start=42.5"
            )]
        );
        assert_eq!(e.state().state, PlayState::Playing);
    }

    #[tokio::test]
    async fn load_resume_and_preload_pass_per_file_gain() {
        let fake = FakeSpawner::default();
        let paths = paths(&["a", "b", "c"]);
        let r = Gained(
            &paths,
            HashMap::from([("a", -6.5), ("b", 3.254), ("c", -0.001)]),
        );
        let (mut e, _rx) = engine(&fake, LoopMode::None);
        e.play(ContextRef::Album(1), ids(&["a", "b", "c"]), "a", &r)
            .await
            .unwrap();
        let g = fake.generation();
        e.on_mpv_event(g, MpvEvent::FileLoaded, &r).await;
        assert_eq!(
            fake.commands(),
            vec![
                loadfile_opts(&p("a"), "replace", "volume-gain=-6.50"),
                cmd(&[json!("playlist-clear")]),
                loadfile_opts(&p("b"), "append", "volume-gain=3.25"),
            ]
        );

        // The preloaded entry's gain becomes the current one: an idle quit
        // and resume reload `b` with it, at the position.
        fake.clear_commands();
        assert!(e.on_mpv_event(g, prop("path", json!(p("b"))), &r).await);
        assert_eq!(
            fake.commands(),
            vec![
                cmd(&[json!("playlist-clear")]),
                loadfile_opts(&p("c"), "append", "volume-gain=0.00"),
            ]
        );
        fake.set_time_pos(12.3);
        e.pause().await.unwrap();
        e.on_mpv_event(g, prop("pause", json!(true)), &r).await;
        e.on_idle_deadline().await;
        fake.clear_commands();
        e.resume().await.unwrap();
        assert_eq!(
            fake.commands(),
            vec![loadfile_opts(
                &p("b"),
                "replace",
                "volume-gain=3.25,start=12.3"
            )]
        );

        // Manual steps load with the target's gain.
        fake.set_time_pos(1.0);
        e.on_mpv_event(fake.generation(), MpvEvent::PlaybackRestart, &r)
            .await;
        fake.clear_commands();
        e.prev(&r).await.unwrap();
        assert_eq!(
            fake.commands().last().unwrap(),
            &loadfile_opts(&p("a"), "replace", "volume-gain=-6.50")
        );
    }

    #[tokio::test]
    async fn new_gain_re_preloads_next_but_leaves_the_playing_track() {
        let fake = FakeSpawner::default();
        let paths = paths(&["a", "b"]);
        let before = Gained(&paths, HashMap::from([("a", 1.0), ("b", 2.0)]));
        let mut e = playing(&fake, LoopMode::None, &["a", "b"], "a", &paths).await;
        e.refresh_gains(&before).await.unwrap();
        fake.clear_commands();

        // Same gains: nothing to do.
        e.refresh_gains(&before).await.unwrap();
        assert!(fake.commands().is_empty());

        // `a` (playing) and `b` (next) were measured: only `b` is reloaded.
        let after = Gained(&paths, HashMap::from([("a", -4.0), ("b", -2.5)]));
        e.refresh_gains(&after).await.unwrap();
        assert_eq!(
            fake.commands(),
            vec![
                cmd(&[json!("playlist-clear")]),
                loadfile_opts(&p("b"), "append", "volume-gain=-2.50"),
            ]
        );
        assert_eq!(e.preloaded_id(), Some("b"));
    }

    #[tokio::test]
    async fn toggling_leveling_sets_gain_live_and_re_preloads() {
        let fake = FakeSpawner::default();
        let paths = paths(&["a", "b"]);
        let on = Gained(&paths, HashMap::from([("a", -6.0), ("b", 4.5)]));
        let (mut e, _rx) = engine(&fake, LoopMode::None);
        e.play(ContextRef::Album(1), ids(&["a", "b"]), "a", &on)
            .await
            .unwrap();
        e.on_mpv_event(fake.generation(), MpvEvent::FileLoaded, &on)
            .await;
        fake.clear_commands();

        let off = Gained(&paths, HashMap::new());
        e.apply_leveling(&off).await.unwrap();
        assert_eq!(
            fake.commands(),
            vec![
                cmd(&[json!("set_property"), json!("volume-gain"), json!(0.0)]),
                cmd(&[json!("playlist-clear")]),
                loadfile_opts(&p("b"), "append", "volume-gain=0.00"),
            ]
        );

        fake.clear_commands();
        e.pause().await.unwrap();
        fake.clear_commands();
        e.apply_leveling(&on).await.unwrap();
        assert_eq!(
            fake.commands(),
            vec![
                cmd(&[json!("set_property"), json!("volume-gain"), json!(-6.0)]),
                cmd(&[json!("playlist-clear")]),
                loadfile_opts(&p("b"), "append", "volume-gain=4.50"),
            ]
        );

        // Stopped: nothing is sent; the next play uses the new gain.
        e.stop().await.unwrap();
        fake.clear_commands();
        e.apply_leveling(&off).await.unwrap();
        assert!(fake.commands().is_empty());
    }

    #[tokio::test]
    async fn unexpected_exit_stops_and_next_play_respawns() {
        let fake = FakeSpawner::default();
        let r = paths(&["a"]);
        let (mut e, mut rx) = engine(&fake, LoopMode::None);
        e.play(ContextRef::Album(1), ids(&["a"]), "a", &r)
            .await
            .unwrap();
        assert_eq!(fake.generation(), 1);

        fake.emit(MpvEvent::Exited);
        let (g, ev) = rx.recv().await.unwrap();
        assert!(e.on_mpv_event(g, ev, &r).await);
        assert_eq!(e.state().state, PlayState::Stopped);
        assert!(e.idle_deadline().is_none());

        e.play(ContextRef::Album(1), ids(&["a"]), "a", &r)
            .await
            .unwrap();
        assert_eq!(fake.spawns(), 2);
        assert_eq!(fake.generation(), 2);
        assert_eq!(e.state().state, PlayState::Playing);
    }

    #[tokio::test]
    async fn two_consecutive_timeouts_kill_mpv_and_the_next_command_respawns() {
        let fake = FakeSpawner::default();
        let r = paths(&["a", "b"]);
        let mut e = playing(&fake, LoopMode::None, &["a", "b"], "a", &r).await;

        // A reply in between resets the count.
        fake.set_hung(true);
        assert!(e.pause().await.is_err());
        fake.set_hung(false);
        e.set_volume(40.0).await.unwrap();
        fake.set_hung(true);
        assert!(e.pause().await.is_err());
        assert_eq!(fake.kills(), 0);
        assert_eq!(e.state().state, PlayState::Playing);
        assert_eq!(e.take_warning(), None);

        // The second timeout in a row kills it; the queue stays on `a`.
        assert!(e.next(&r).await.is_err());
        assert_eq!(fake.kills(), 1);
        let st = e.state();
        assert_eq!(st.state, PlayState::Stopped);
        assert_eq!(st.track_id.as_deref(), Some("a"));
        assert_eq!(
            e.take_warning().as_deref(),
            Some("The player stopped responding and was restarted")
        );

        fake.set_hung(false);
        fake.clear_commands();
        e.toggle().await.unwrap();
        assert_eq!(fake.spawns(), 2);
        assert_eq!(fake.commands(), vec![loadfile(&p("a"), "replace")]);
        assert_eq!(e.state().state, PlayState::Playing);
    }

    struct CountingSpawner {
        inner: ProcessSpawner,
        spawns: Arc<AtomicUsize>,
    }

    impl MpvSpawner for CountingSpawner {
        type Mpv = MpvProcess;

        async fn spawn(
            &self,
            volume: f64,
            generation: u64,
            events: mpsc::UnboundedSender<(u64, MpvEvent)>,
        ) -> anyhow::Result<MpvProcess> {
            self.spawns.fetch_add(1, Ordering::SeqCst);
            self.inner.spawn(volume, generation, events).await
        }
    }

    #[tokio::test]
    #[ignore = "needs real mpv"]
    async fn real_mpv_gapless_transition_uses_one_process() {
        let dir = tempfile::tempdir().unwrap();
        let spawns = Arc::new(AtomicUsize::new(0));
        let spawner = CountingSpawner {
            inner: ProcessSpawner {
                opts_template: MpvOptions {
                    socket_path: dir.path().join("mpv.sock"),
                    ao: None,
                    volume: 0.0,
                    extra_args: vec!["--ao=null".into()],
                },
            },
            spawns: spawns.clone(),
        };
        let fixture = |name: &str| format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let r = Paths(HashMap::from([
            ("a".to_string(), fixture("tone.opus")),
            ("b".to_string(), fixture("tone2.opus")),
        ]));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut e = Engine::new(spawner, tx, 50.0, LoopMode::None);
        e.play(ContextRef::Album(1), ids(&["a", "b"]), "a", &r)
            .await
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(15);
        while e.state().track_id.as_deref() == Some("a") {
            let (g, ev) = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("no transition to the second track")
                .unwrap();
            let is_b_path = ev == prop("path", json!(fixture("tone2.opus")));
            e.on_mpv_event(g, ev, &r).await;
            if e.state().track_id.as_deref() == Some("b") {
                assert!(is_b_path, "advanced without the path change");
            }
        }
        assert_eq!(e.state().state, PlayState::Playing);

        e.stop().await.unwrap();
        assert_eq!(e.state().state, PlayState::Stopped);
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        e.on_idle_deadline().await;
    }
}
