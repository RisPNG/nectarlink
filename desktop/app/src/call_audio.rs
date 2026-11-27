// SPDX-License-Identifier: GPL-3.0-or-later
//! A phone call's sound on this PC (docs/protocol/calls.md §2.6): the other
//! party plays from the speakers and the PC's microphone goes into the call,
//! while the phone shows that its call sounds here.

use std::sync::{Arc, Mutex};

use nectarlink_core::{DeviceId, MicSink, MirrorAudioConfig};

use crate::{
    bridge::app::{describe, show_message},
    core_host,
    i18n::tr,
    state::Changes,
};

/// Where the call's sound is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Off,
    /// Asked the phone; waiting for it.
    Starting(DeviceId),
    /// The call sounds here.
    Live(DeviceId),
}

impl Phase {
    pub fn device(self) -> Option<DeviceId> {
        match self {
            Phase::Off => None,
            Phase::Starting(d) | Phase::Live(d) => Some(d),
        }
    }
}

#[derive(Default)]
struct State {
    phase: Phase,
    /// The PC's microphone, going into the call.
    #[cfg(any(windows, target_os = "linux"))]
    recorder: Option<crate::win::audio_in::Recorder>,
    uplink: Option<Arc<nectarlink_core::MirrorStream>>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn state<T>(f: impl FnOnce(&mut State) -> T) -> T {
    f(STATE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_default())
}

pub fn phase() -> Phase {
    state(|s| s.phase)
}

/// Whether this PC can carry calls from this phone over the paired connection.
pub fn supported(device: DeviceId) -> bool {
    cfg!(any(windows, target_os = "linux"))
        && core_host::host().hub.read(|s| {
            s.matrices.get(&device).and_then(|m| m.state("calls.audio"))
                == Some(nectarlink_core::FeatureState::Available)
        })
}

/// Whether this PC can take `device`'s call in progress.
pub fn available(device: DeviceId) -> bool {
    supported(device) && crate::calls::active(device).is_some()
}

/// Moves `device`'s call in progress here.
pub fn start(device: DeviceId) {
    let Some(node) = core_host::node() else { return };
    let Some(call) = crate::calls::active(device) else { return };
    if !available(device) || phase() != Phase::Off {
        return;
    }
    set_phase(Phase::Starting(device));
    core_host::spawn(async move {
        if let Err(e) = node.call_audio_start(device, call.id).await {
            tracing::info!(error = %e, "the phone didn't move the call's sound");
            end(device);
            show_message(match e {
                nectarlink_core::Error::Unsupported => {
                    tr!("Calls", "This phone doesn't let NectarLink reach its calls' sound.")
                }
                nectarlink_core::Error::NotFound => tr!("Calls", "That call is over."),
                e => describe(&e),
            });
            return;
        }
        let uplink = match node.call_audio_open(device, true).await {
            Ok(stream) => Arc::new(stream),
            Err(e) => {
                tracing::warn!(error = %e, "can't send the PC's microphone into the call");
                stop(device);
                show_message(describe(&e));
                return;
            }
        };
        // Sending on a MirrorStream and starting the recorder block: off the runtime's threads.
        let recording = uplink.clone();
        let recorded = tokio::task::spawn_blocking(move || record_into(&recording, device))
            .await
            .unwrap_or_else(|e| Err(MicError::Other(e.to_string())));
        if let Err(e) = recorded {
            tracing::warn!(error = %e, "can't record the PC's microphone");
            stop(device);
            show_message(mic_message(&e));
            return;
        }
        state(|s| {
            if s.phase == Phase::Starting(device) {
                s.uplink = Some(uplink.clone());
                s.phase = Phase::Live(device);
            }
        });
        if phase() != Phase::Live(device) {
            // Stopped meanwhile.
            uplink.close();
        }
        core_host::host().hub.changed(Changes::CALLS);
    });
}

#[cfg(any(windows, target_os = "linux"))]
use crate::win::audio_in::MicError;

/// Why the microphone can't be recorded on an unsupported platform.
#[cfg(not(any(windows, target_os = "linux")))]
#[derive(Debug)]
pub enum MicError {
    Other(String),
}

#[cfg(not(any(windows, target_os = "linux")))]
impl std::fmt::Display for MicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let MicError::Other(e) = self;
        f.write_str(e)
    }
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

#[cfg(any(windows, target_os = "linux"))]
fn record_into(uplink: &Arc<nectarlink_core::MirrorStream>, device: DeviceId) -> Result<(), MicError> {
    let rate = nectarlink_core::CALLS_AUDIO_RATE;
    let config = MirrorAudioConfig { codec: nectarlink_core::MIRROR_PCM.into(), rate, channels: 1 };
    uplink.send(nectarlink_core::PacketKind::Config, 0, config.to_cbor());
    let recorder = recorder(uplink, chosen_mic())?;
    state(|s| {
        if s.phase == Phase::Starting(device) {
            s.recorder = Some(recorder);
        }
    });
    Ok(())
}

/// Records the chosen microphone into `uplink`. Blocks a moment.
#[cfg(any(windows, target_os = "linux"))]
fn recorder(
    uplink: &Arc<nectarlink_core::MirrorStream>,
    mic: Option<String>,
) -> Result<crate::win::audio_in::Recorder, MicError> {
    let rate = nectarlink_core::CALLS_AUDIO_RATE;
    let stream = uplink.clone();
    let mut samples = 0u64;
    crate::win::audio_in::Recorder::start(mic, rate, move |pcm| {
        let time_us = samples * 1_000_000 / u64::from(rate);
        samples += pcm.len() as u64 / 2;
        stream.send(nectarlink_core::PacketKind::Frame, time_us, pcm);
    })
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
/// it's there, otherwise the platform default.
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
        if current.as_deref() == Some(id.as_str()) {
            return;
        }
        *current = Some(id.clone());
    }
    let dir = &core_host::host().data_dir;
    let mut settings = crate::settings::Settings::load(dir);
    settings.call_mic = Some(id);
    if let Err(e) = settings.save(dir) {
        tracing::warn!(error = %e, "can't save the call microphone");
    }
    #[cfg(windows)]
    switch_mic();
    #[cfg(target_os = "linux")]
    core_host::spawn(async {
        let _ = tokio::task::spawn_blocking(switch_mic).await;
    });
    core_host::host().hub.changed(Changes::CALLS);
}

/// A call sounding here moves to the microphone just chosen.
#[cfg(any(windows, target_os = "linux"))]
fn switch_mic() {
    let selected = chosen_mic();
    let (old, uplink) = state(|s| match s.phase {
        Phase::Live(_) => (s.recorder.take(), s.uplink.clone()),
        _ => (None, None),
    });
    let Some(uplink) = uplink else { return };
    drop(old);
    match recorder(&uplink, selected.clone()) {
        Ok(recorder) => state(|s| {
            if s.uplink.as_ref().is_some_and(|current| Arc::ptr_eq(current, &uplink))
                && s.recorder.is_none()
                && chosen_mic() == selected
            {
                s.recorder = Some(recorder);
            }
        }),
        Err(e) if chosen_mic() == selected => {
            tracing::warn!(error = %e, "can't record the chosen microphone");
            show_message(mic_message(&e));
        }
        Err(_) => {}
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn record_into(_uplink: &Arc<nectarlink_core::MirrorStream>, _device: DeviceId) -> Result<(), MicError> {
    Err(MicError::Other("not on this system".into()))
}

/// Gives the call's sound back to `device` and tells it.
pub fn stop(device: DeviceId) {
    if !end(device) {
        return;
    }
    let Some(node) = core_host::node() else { return };
    core_host::spawn(async move { node.call_audio_stop(device).await });
}

/// Ends the call's sound here (the call ended, the phone took it back or
/// went away); whether it was `device`'s.
pub fn end(device: DeviceId) -> bool {
    // The recorder stops when `_recorder` goes, at the end: outside the lock,
    // which its thread may be waiting on.
    let (_recorder, uplink) = {
        let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
        let s = guard.get_or_insert_default();
        if s.phase.device() != Some(device) {
            return false;
        }
        s.phase = Phase::Off;
        #[cfg(any(windows, target_os = "linux"))]
        let recorder = s.recorder.take();
        #[cfg(not(any(windows, target_os = "linux")))]
        let recorder = ();
        (recorder, s.uplink.take())
    };
    if let Some(uplink) = uplink {
        uplink.close();
    }
    core_host::host().hub.changed(Changes::CALLS);
    true
}

fn set_phase(phase: Phase) {
    state(|s| s.phase = phase);
    core_host::host().hub.changed(Changes::CALLS);
}

/// Where the other party plays: only while this PC asked `peer` for the call.
pub fn sink(peer: &DeviceId) -> Option<Arc<dyn MicSink>> {
    if phase().device() != Some(*peer) {
        return None;
    }
    Some(Arc::new(Sink { peer: *peer, player: Mutex::new(None) }))
}

struct Sink {
    peer: DeviceId,
    #[cfg(any(windows, target_os = "linux"))]
    player: Mutex<Option<crate::win::audio_out::Player>>,
    #[cfg(not(any(windows, target_os = "linux")))]
    player: Mutex<Option<()>>,
}

impl MicSink for Sink {
    fn config(&self, config: MirrorAudioConfig) {
        // Plays on the default output; on speakers the microphone hears
        // the other party too (no echo cancelling), so headphones work best.
        #[cfg(any(windows, target_os = "linux"))]
        match crate::win::audio_out::Player::start(config.rate, u16::from(config.channels)) {
            Ok(player) => *self.player.lock().unwrap_or_else(|e| e.into_inner()) = Some(player),
            Err(e) => tracing::warn!(error = %e, "can't play the call"),
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = config;
    }

    fn audio(&self, _time_us: u64, data: Vec<u8>) {
        #[cfg(any(windows, target_os = "linux"))]
        if let Some(player) = self.player.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            player.push(data);
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = data;
    }

    /// The phone stopped sending: the call ended or it took the sound back.
    fn ended(&self) {
        self.player.lock().unwrap_or_else(|e| e.into_inner()).take();
        end(self.peer);
    }
}
