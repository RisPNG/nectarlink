// SPDX-License-Identifier: GPL-3.0-or-later
//! A phone call's sound on this PC (docs/protocol/calls.md §2.6): the other
//! party plays from the speakers and the PC's microphone goes into the call,
//! while the phone shows that its call sounds here.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use nectarlink_core::{DeviceId, FeatureState, MicSink, MirrorAudioConfig, MirrorStream};

use crate::{
    bridge::app::{describe, show_message},
    core_host,
    i18n::tr,
    state::Changes,
};

#[cfg(any(windows, target_os = "linux"))]
use crate::win::audio_in::{MicError, Recorder};

#[cfg(not(any(windows, target_os = "linux")))]
type Recorder = ();

#[cfg(not(any(windows, target_os = "linux")))]
#[derive(Debug)]
enum MicError {
    Other(String),
}

#[cfg(not(any(windows, target_os = "linux")))]
impl std::fmt::Display for MicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let MicError::Other(e) = self;
        f.write_str(e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Route {
    device: DeviceId,
    request: u64,
}

struct CallAttempt {
    id: String,
    state: AttemptState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptState {
    Tried,
    Disconnected,
    ReturnedToPhone,
}

#[derive(Debug)]
pub enum Departure {
    Finished,
    Disconnected,
    ReturnedToPhone,
}

#[derive(Default)]
struct State {
    route: Option<Route>,
    next_request: u64,
    /// Avoids repeated automatic requests for the same call on this connection.
    attempted_calls: HashMap<DeviceId, CallAttempt>,
    recorder: Option<Recorder>,
    uplink: Option<Arc<MirrorStream>>,
}

impl State {
    fn call_departed(&mut self, device: DeviceId, departure: Departure) -> Option<Route> {
        match departure {
            Departure::Finished => {
                self.attempted_calls.remove(&device);
            }
            Departure::Disconnected => {
                if let Some(call) = self.attempted_calls.get_mut(&device)
                    && call.state != AttemptState::ReturnedToPhone
                {
                    call.state = AttemptState::Disconnected;
                }
            }
            Departure::ReturnedToPhone => {
                if let Some(call) = self.attempted_calls.get_mut(&device) {
                    call.state = AttemptState::ReturnedToPhone;
                }
            }
        }
        self.route.filter(|r| r.device == device)
    }
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
/// Serialize commands so a late stop cannot end a newer route.
static REMOTE: tokio::sync::Mutex<Option<Route>> = tokio::sync::Mutex::const_new(None);

fn state<T>(f: impl FnOnce(&mut State) -> T) -> T {
    f(STATE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_default())
}

pub fn device() -> Option<DeviceId> {
    state(|s| s.route.map(|r| r.device))
}

/// Whether this PC can carry calls from this phone over the paired connection.
pub fn supported(device: DeviceId) -> bool {
    cfg!(any(windows, target_os = "linux"))
        && core_host::node()
            .and_then(|n| n.capabilities(device).ok())
            .is_some_and(|matrix| matrix.state("calls.audio") == Some(FeatureState::Available))
}

/// Moves the active cellular call here. Duplicate state events may ask automatically.
pub fn start(device: DeviceId, automatic: bool) {
    let Some(node) = core_host::node() else { return };
    let Some(call) = crate::calls::active(device).filter(|c| c.app.is_none()) else { return };
    if !supported(device) {
        return;
    }
    let route = state(|s| {
        let attempt = s.attempted_calls.get(&device);
        let reconnecting = attempt.is_some_and(|a| a.state == AttemptState::Disconnected);
        if s.route.is_some() || (automatic && !reconnecting && attempt.is_some_and(|a| a.id == call.id)) {
            return None;
        }
        let route = Route { device, request: s.next_request };
        s.next_request = s.next_request.wrapping_add(1);
        s.route = Some(route);
        s.attempted_calls.insert(device, CallAttempt { id: call.id.clone(), state: AttemptState::Tried });
        Some((route, reconnecting))
    });
    let Some((route, reconnecting)) = route else { return };
    core_host::host().hub.changed(Changes::CALLS);
    core_host::spawn(async move {
        let mut remote = REMOTE.lock().await;
        if state(|s| s.route != Some(route)) {
            return;
        }
        if let Some(previous) = remote.take()
            && (!reconnecting || previous.device != device)
        {
            node.call_audio_stop(previous.device).await;
        }
        if state(|s| s.route != Some(route)) {
            return;
        }
        if let Err(e) = node.call_audio_start(device, call.id).await {
            tracing::info!(peer = %device, error = %e, "the phone didn't move the call's sound");
            if end_route(route) {
                node.call_audio_stop(device).await;
                show_message(match e {
                    nectarlink_core::Error::Unsupported => {
                        tr!("Calls", "This phone doesn't let NectarLink reach its calls' sound.")
                    }
                    nectarlink_core::Error::NotFound => tr!("Calls", "That call is over."),
                    e => describe(&e),
                });
            }
            return;
        }
        *remote = Some(route);
        drop(remote);
        if state(|s| s.route != Some(route)) {
            return;
        }
        let uplink = match node.call_audio_open(device, true).await {
            Ok(stream) => Arc::new(stream),
            Err(e) => {
                tracing::warn!(peer = %device, error = %e, "can't send the PC's microphone into the call");
                if stop_route(route) {
                    show_message(describe(&e));
                }
                return;
            }
        };
        let recording = uplink.clone();
        let recorded = tokio::task::spawn_blocking(move || {
            let config = MirrorAudioConfig {
                codec: nectarlink_core::MIRROR_PCM.into(),
                rate: nectarlink_core::CALLS_AUDIO_RATE,
                channels: 1,
            };
            recording.send(nectarlink_core::PacketKind::Config, 0, config.to_cbor());
            recorder(&recording)
        })
        .await
        .unwrap_or_else(|e| Err(MicError::Other(e.to_string())));
        match recorded {
            Ok(recorder) => {
                let mut recorder = Some(recorder);
                state(|s| {
                    if s.route == Some(route) {
                        s.recorder = recorder.take();
                        s.uplink = Some(uplink.clone());
                    }
                });
                if recorder.is_some() {
                    uplink.close();
                }
            }
            Err(e) => {
                tracing::warn!(peer = %device, error = %e, "can't record the PC's microphone");
                uplink.close();
                if stop_route(route) {
                    show_message(mic_message(&e));
                }
            }
        }
    });
}

/// What to tell the person when the microphone can't be recorded.
fn mic_message(e: &MicError) -> String {
    match e {
        #[cfg(windows)]
        MicError::Denied => tr!(
            "Calls",
            "Windows keeps NectarLink off the microphone. Turn on Settings › Privacy & security › Microphone › Let desktop apps access your microphone, then try again."
        ),
        #[cfg(any(windows, target_os = "linux"))]
        MicError::Missing => tr!("Calls", "No microphone on this PC to talk with."),
        MicError::Other(_) => tr!(
            "Calls",
            "The PC's microphone couldn't be started. Pick another one in Settings › Camera & Audio › Microphone for calls, or try again."
        ),
    }
}

/// Records the chosen microphone into `uplink`. Blocks until capture starts.
#[cfg(any(windows, target_os = "linux"))]
fn recorder(uplink: &Arc<MirrorStream>) -> Result<Recorder, MicError> {
    let rate = nectarlink_core::CALLS_AUDIO_RATE;
    let stream = uplink.clone();
    let mut samples = 0u64;
    Recorder::start(chosen_mic(), rate, move |pcm| {
        let time_us = samples * 1_000_000 / u64::from(rate);
        samples += pcm.len() as u64 / 2;
        stream.send(nectarlink_core::PacketKind::Frame, time_us, pcm);
    })
}

#[cfg(not(any(windows, target_os = "linux")))]
fn recorder(_uplink: &Arc<MirrorStream>) -> Result<Recorder, MicError> {
    Err(MicError::Other("not on this system".into()))
}

/// The microphone chosen in Settings, if any (kept when other settings save).
static MIC: Mutex<Option<String>> = Mutex::new(None);

pub fn init(settings: &crate::settings::Settings) {
    *MIC.lock().unwrap_or_else(|e| e.into_inner()) = settings.call_mic.clone();
}

pub fn chosen_mic() -> Option<String> {
    MIC.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The PC's microphones, and the one calls record: the chosen one while
/// it's there, else the platform default.
pub fn mics() -> (Vec<nectarlink_core::AudioOutputDevice>, Option<String>) {
    #[cfg(any(windows, target_os = "linux"))]
    let devices = crate::win::audio::input_devices();
    #[cfg(not(any(windows, target_os = "linux")))]
    let devices: Vec<nectarlink_core::AudioOutputDevice> = Vec::new();
    let chosen = chosen_mic().filter(|id| devices.iter().any(|d| &d.id == id));
    let current = chosen.or_else(|| devices.iter().find(|d| d.is_default).map(|d| d.id.clone()));
    #[cfg(target_os = "linux")]
    let current = current.or_else(|| devices.first().map(|d| d.id.clone()));
    (devices, current)
}

/// Calls record this microphone from now on, the one in progress too.
pub fn set_mic(id: String) {
    {
        let mut current = MIC.lock().unwrap_or_else(|e| e.into_inner());
        if current.as_ref() == Some(&id) {
            return;
        }
        *current = Some(id.clone());
    }
    let dir = &core_host::host().data_dir;
    if let Err(e) = crate::settings::Settings::update(dir, |settings| settings.call_mic = Some(id)) {
        tracing::warn!(error = %e, "can't save the call microphone");
    }
    #[cfg(any(windows, target_os = "linux"))]
    core_host::spawn(async {
        let _ = tokio::task::spawn_blocking(switch_mic).await;
    });
    core_host::host().hub.changed(Changes::CALLS);
}

/// A call sounding here moves to the microphone just chosen.
#[cfg(any(windows, target_os = "linux"))]
fn switch_mic() {
    let Some((route, old, uplink)) = state(|s| Some((s.route?, s.recorder.take()?, s.uplink.clone()?)))
    else {
        return;
    };
    drop(old);
    match recorder(&uplink) {
        Ok(recorder) => {
            let mut recorder = Some(recorder);
            state(|s| {
                if s.route == Some(route) && s.recorder.is_none() {
                    s.recorder = recorder.take();
                }
            });
        }
        Err(e) => {
            tracing::warn!(error = %e, "can't record the chosen microphone");
            if stop_route(route) {
                show_message(mic_message(&e));
            }
        }
    }
}

/// Gives the call's sound back to `device` and tells it.
pub fn stop(device: DeviceId) {
    if let Some(route) = state(|s| s.call_departed(device, Departure::ReturnedToPhone)) {
        stop_route(route);
    }
}

fn stop_route(route: Route) -> bool {
    if !end_route(route) {
        return false;
    }
    let Some(node) = core_host::node() else { return true };
    core_host::spawn(async move {
        let mut remote = REMOTE.lock().await;
        if *remote == Some(route) {
            *remote = None;
            node.call_audio_stop(route.device).await;
        }
    });
    true
}

/// Ends the local route and remembers whether another automatic attempt is welcome.
pub fn end(device: DeviceId, departure: Departure) {
    if let Some(route) = state(|s| s.call_departed(device, departure)) {
        end_route(route);
    }
}

fn end_route(route: Route) -> bool {
    let Some((_recorder, uplink)) = state(|s| {
        if s.route != Some(route) {
            return None;
        }
        s.route = None;
        Some((s.recorder.take(), s.uplink.take()))
    }) else {
        return false;
    };
    if let Some(uplink) = uplink {
        uplink.close();
    }
    core_host::host().hub.changed(Changes::CALLS);
    true
}

/// Where the other party plays, while this PC has asked `peer` for the call.
pub fn sink(peer: &DeviceId) -> Option<Arc<dyn MicSink>> {
    let route = state(|s| s.route.filter(|r| &r.device == peer))?;
    Some(Arc::new(Sink { route, player: Mutex::new(None) }))
}

struct Sink {
    route: Route,
    #[cfg(any(windows, target_os = "linux"))]
    player: Mutex<Option<crate::win::audio_out::Player>>,
    #[cfg(not(any(windows, target_os = "linux")))]
    player: Mutex<Option<()>>,
}

impl MicSink for Sink {
    fn config(&self, config: MirrorAudioConfig) {
        #[cfg(any(windows, target_os = "linux"))]
        if state(|s| s.route == Some(self.route)) {
            match crate::win::audio_out::Player::start(config.rate, u16::from(config.channels)) {
                Ok(player) => *self.player.lock().unwrap_or_else(|e| e.into_inner()) = Some(player),
                Err(e) => {
                    tracing::warn!(error = %e, "can't play the call");
                    if stop_route(self.route) {
                        show_message(tr!("Calls", "The PC couldn't play the call."));
                    }
                }
            }
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = config;
    }

    fn audio(&self, _time_us: u64, mut data: Vec<u8>) {
        #[cfg(any(windows, target_os = "linux"))]
        if state(|s| s.route == Some(self.route))
            && let Some(player) = self.player.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
        {
            crate::phone_sound::apply_volume(&mut data);
            player.push(data);
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = &mut data;
    }

    fn ended(&self) {
        self.player.lock().unwrap_or_else(|e| e.into_inner()).take();
        end_route(self.route);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_connection_retries_the_call_unless_its_sound_was_returned() {
        let device = DeviceId([1; 32]);
        let route = Route { device, request: 1 };
        let mut state = State { route: Some(route), ..State::default() };
        state
            .attempted_calls
            .insert(device, CallAttempt { id: "current".into(), state: AttemptState::Tried });
        assert_eq!(state.call_departed(device, Departure::Disconnected), Some(route));
        assert_eq!(state.attempted_calls[&device].state, AttemptState::Disconnected);

        state
            .attempted_calls
            .insert(device, CallAttempt { id: "current".into(), state: AttemptState::Tried });
        assert_eq!(state.call_departed(device, Departure::ReturnedToPhone), Some(route));
        state.route = None;
        assert_eq!(state.call_departed(device, Departure::Disconnected), None);
        assert_eq!(state.attempted_calls[&device].id, "current");
        assert_eq!(state.attempted_calls[&device].state, AttemptState::ReturnedToPhone);
    }
}
