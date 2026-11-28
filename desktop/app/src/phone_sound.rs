// SPDX-License-Identifier: GPL-3.0-or-later
//! The phone's sound on this PC without a mirror window
//! (`MirrorStart::sound_only`, docs/protocol/mirror.md): what the phone plays
//! comes out of the PC's audio output. Opening the phone's mirror window
//! takes over playback, keeping the same volume and output.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use nectarlink_core::{
    DeviceId, Error, FeatureState, LinkState, MIRROR_SCREEN, MirrorAudioConfig, MirrorConfig, MirrorSink,
    MirrorStart, NodeEvent,
};

use crate::{
    bridge::app::show_message,
    core_host,
    i18n::tr,
    state::{AppState, Changes},
    win::audio_out::Player,
};

/// How long the phone's prompt may go unanswered.
const ASK_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Asked; the phone may be showing its prompt.
    Asking,
    /// Its sound is playing here.
    Playing,
    /// Media playback ended, but this PC is still taking the phone's calls.
    Ready,
    /// The screen mirror took over the sound, while call routing stayed on.
    Mirroring,
    /// The user asked for sound before the connection was lost.
    Reconnecting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Session {
    device: DeviceId,
    request: u64,
    phase: Phase,
    for_apps: bool,
    /// Explicit phone sound has played for this request or one it resumed.
    calls_armed: bool,
}

impl Session {
    fn ask(device: DeviceId, for_apps: bool, previous: Option<Self>) -> Self {
        Self {
            device,
            request: NEXT_REQUEST.fetch_add(1, Ordering::Relaxed),
            phase: Phase::Asking,
            for_apps,
            calls_armed: !for_apps && previous.is_some_and(|session| session.calls_armed),
        }
    }
}

/// The one phone whose sound plays here, if any.
static ACTIVE: Mutex<Option<Session>> = Mutex::new(None);
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(0);
/// Serialize remote stop/start commands, including rapid source changes.
static REMOTE: tokio::sync::Mutex<Option<Session>> = tokio::sync::Mutex::const_new(None);

fn active() -> Option<Session> {
    *ACTIVE.lock().unwrap_or_else(|e| e.into_inner())
}

fn changed() {
    // Home's switch, and app windows' sound button.
    core_host::host().hub.changed(Changes::MEDIA | Changes::MIRROR);
}

fn is_current(session: Session) -> bool {
    active().is_some_and(|s| s.device == session.device && s.request == session.request)
}

/// Retires one stream by request ID, preserving an explicit request when it can still be used.
fn retire_stream(
    active: &mut Option<Session>,
    expected: Session,
    phase: Option<Phase>,
    next: Option<Phase>,
) -> Option<Session> {
    let current = active.filter(|s| {
        s.device == expected.device
            && s.request == expected.request
            && phase.is_none_or(|phase| s.phase == phase)
    })?;
    *active = next
        .filter(|next| !current.for_apps && (*next == Phase::Reconnecting || current.calls_armed))
        .map(|phase| Session { phase, ..current });
    Some(current)
}

fn clear_current(session: Session, phase: Option<Phase>, next: Option<Phase>) -> Option<Session> {
    let current = retire_stream(
        &mut ACTIVE.lock().unwrap_or_else(|e| e.into_inner()),
        session,
        phase,
        next.filter(|phase| *phase == Phase::Reconnecting || cfg!(any(windows, target_os = "linux"))),
    )?;
    changed();
    Some(current)
}

fn link_lost(active: &mut Option<Session>, device: DeviceId) -> bool {
    let Some(session) =
        active.filter(|s| s.device == device && !matches!(s.phase, Phase::Mirroring | Phase::Reconnecting))
    else {
        return false;
    };
    *active = (!session.for_apps).then(|| Session {
        request: NEXT_REQUEST.fetch_add(1, Ordering::Relaxed),
        phase: Phase::Reconnecting,
        ..session
    });
    true
}

// ---- How loud it plays here (#577) ----

/// The gain for `level` (`0..=100`): squared, so the slider's steps sound
/// even, the way a volume control feels.
fn gain(level: u8) -> f32 {
    let x = f32::from(level.min(100)) / 100.0;
    x * x
}

/// Applies the phone sound level to the PCM for media and routed calls.
pub(crate) fn apply_volume(data: &mut [u8]) {
    scale(data, crate::settings::phone_sound_volume());
}

pub(crate) struct Playback {
    player: Player,
    #[cfg(windows)]
    format: MirrorAudioConfig,
    #[cfg(windows)]
    output: tokio::sync::watch::Receiver<Option<String>>,
}

impl Playback {
    pub(crate) fn start(format: MirrorAudioConfig) -> Option<Self> {
        #[cfg(windows)]
        let mut output = crate::settings::PHONE_SOUND_OUTPUT.subscribe();
        #[cfg(windows)]
        let device = output.borrow_and_update().clone();
        #[cfg(not(windows))]
        let device = None;
        Some(Self {
            player: Self::open(&format, device)?,
            #[cfg(windows)]
            format,
            #[cfg(windows)]
            output,
        })
    }

    fn open(format: &MirrorAudioConfig, device: Option<String>) -> Option<Player> {
        let channels = u16::from(format.channels);
        match device {
            None => Player::start(format.rate, channels),
            device => Player::start_on(device, format.rate, channels),
        }
        .inspect_err(|e| tracing::warn!(error = %e, "can't play the phone's sound"))
        .ok()
    }

    pub(crate) fn push(&mut self, mut data: Vec<u8>) {
        #[cfg(windows)]
        if self.output.has_changed().unwrap_or_default() {
            let device = self.output.borrow_and_update().clone();
            if let Some(player) = Self::open(&self.format, device) {
                self.player = player;
            }
        }
        if !crate::mirror::muted() {
            apply_volume(&mut data);
            self.player.push(data);
        }
    }
}

fn scale(data: &mut [u8], level: u8) {
    if level >= 100 {
        return;
    }
    let gain = gain(level);
    let (samples, _) = data.as_chunks_mut::<2>();
    for sample in samples {
        let value = f32::from(i16::from_le_bytes(*sample)) * gain;
        *sample = (value.round() as i16).to_le_bytes();
    }
}

/// Sets playback gain on this PC. `done` saves the final slider value.
pub fn set_volume(level: u8, done: bool) {
    if done {
        crate::settings::remember_phone_sound_volume(level);
    } else {
        crate::settings::set_phone_sound_volume(level);
    }
    changed();
}

/// The selected phone, if it is online and can share its sound alone.
fn phone(device: DeviceId) -> Option<(DeviceId, String)> {
    core_host::host().hub.read(|s| sharing_phone(s, device))
}

fn sharing_phone(s: &AppState, device: DeviceId) -> Option<(DeviceId, String)> {
    let phone = s.devices.iter().find(|d| d.id == device)?;
    (matches!(phone.link, LinkState::Online { .. })
        && s.matrices.get(&device).and_then(|m| m.state("mirroring.sound")) == Some(FeatureState::Available))
    .then(|| (device, phone.info.name.clone()))
}

/// Whether `device`'s sound is playing here without its screen's window.
pub fn playing(device: &DeviceId) -> bool {
    state_for(active(), device) == "playing"
}

/// Whether the user enabled phone sound independently of any mirror window.
pub fn routes_calls(device: DeviceId) -> bool {
    active().is_some_and(|s| s.device == device && s.calls_armed)
}

fn state_for(session: Option<Session>, device: &DeviceId) -> &'static str {
    match session.filter(|s| s.device == *device) {
        None => "off",
        Some(Session { phase: Phase::Asking, .. }) => "asking",
        Some(Session { phase: Phase::Playing, .. }) => "playing",
        Some(Session { phase: Phase::Ready | Phase::Mirroring, .. }) => "ready",
        Some(Session { phase: Phase::Reconnecting, .. }) => "reconnecting",
    }
}

/// The phone's sound request state, including when only call routing remains.
pub fn state(device: &DeviceId) -> &'static str {
    state_for(active(), device)
}

/// Starts or stops playing the selected phone's sound here.
pub fn set_on(device: DeviceId, on: bool) {
    if !on {
        if let Some(session) = active().filter(|s| s.device == device)
            && let Some(session) = clear_current(session, None, None)
        {
            stop_remote(session);
            if !session.for_apps {
                crate::call_audio::stop(device);
            }
        }
        return;
    }
    if let Some((device, name)) = phone(device) {
        start(device, name, false, None);
    }
}

pub fn on_event(event: &NodeEvent) {
    let device = match event {
        NodeEvent::LinkChanged { device, link: LinkState::Offline { .. } } => {
            if link_lost(&mut ACTIVE.lock().unwrap_or_else(|e| e.into_inner()), *device) {
                changed();
            }
            return;
        }
        NodeEvent::DeviceRemoved(device) => {
            if let Some(session) = active().filter(|s| s.device == *device) {
                clear_current(session, None, None);
            }
            return;
        }
        NodeEvent::LinkChanged { device, link: LinkState::Online { .. } } => *device,
        NodeEvent::Capabilities(matrix) => matrix.device,
        _ => return,
    };
    if let Some(session) = active().filter(|s| s.device == device && s.phase == Phase::Reconnecting)
        && let Some((device, name)) = phone(device)
    {
        start(device, name, false, Some(session));
    }
}

/// `device`'s app windows opened (`open`) or all closed (#121): its sound
/// plays here while they're open. Sound turned on here stays as it is.
pub fn for_app_windows(device: DeviceId, open: bool) {
    match active() {
        None if open => {
            let Some((_, name)) = phone(device) else { return };
            start(device, name, true, None);
        }
        Some(session) if session.device == device && !open && session.for_apps => {
            if clear_current(session, None, None).is_some() {
                stop_remote(session);
            }
        }
        _ => {}
    }
}

/// Asks `device` for sound, or resumes an explicit request after reconnecting.
fn start(device: DeviceId, name: String, quiet: bool, reconnecting: Option<Session>) {
    let Some(node) = core_host::node() else { return };
    let (session, previous) = {
        let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        if reconnecting.is_some() && *active != reconnecting {
            return;
        }
        if let Some(current) = *active {
            // App windows must not replace sound explicitly turned on here.
            if quiet {
                return;
            }
            if current.device == device && matches!(current.phase, Phase::Asking | Phase::Playing) {
                *active = Some(Session {
                    for_apps: false,
                    calls_armed: current.calls_armed || current.phase == Phase::Playing,
                    ..current
                });
                drop(active);
                if current.phase == Phase::Playing {
                    crate::call_audio::start(device, true);
                }
                return;
            }
        }
        let session = Session::ask(device, quiet, active.filter(|s| s.device == device));
        let previous = active.replace(session);
        (session, previous)
    };
    if let Some(previous) = previous.filter(|s| s.calls_armed && s.device != device) {
        crate::call_audio::stop(previous.device);
    }
    changed();
    core_host::spawn(async move {
        let mut remote = REMOTE.lock().await;
        if !is_current(session) {
            return;
        }
        if let Some(previous) = remote.take()
            && reconnecting.is_none_or(|session| session.device != previous.device)
        {
            node.mirror_stop(previous.device, MIRROR_SCREEN).await;
        }
        if !is_current(session) {
            return;
        }
        *remote = Some(session);
        let options = MirrorStart {
            max_size: 0,
            fps: 0,
            bitrate: 0,
            audio: true,
            session: MIRROR_SCREEN,
            app: None,
            stay_awake: false,
            screen_off: false,
            watch: None,
            sound_only: true,
            display: None,
        };
        if let Err(e) = node.mirror_start(device, options).await {
            *remote = None;
            let offline = matches!(e, Error::Offline);
            let next = if offline { Phase::Reconnecting } else { Phase::Ready };
            if clear_current(session, Some(Phase::Asking), Some(next)).is_none() || offline {
                return;
            }
            if quiet {
                tracing::info!(error = %e, "the phone's sound didn't come with its app windows");
                return;
            }
            show_message(match e {
                Error::Denied => tr!("PhoneSound", "Mirroring is turned off for this phone."),
                Error::Unsupported => {
                    tr!("PhoneSound", "Update NectarLink on %1 to play its sound here.", name)
                }
                Error::NotPaired => tr!("PhoneSound", "The phone isn't connected."),
                // The phone couldn't show its request (notifications off, say).
                e => {
                    tracing::warn!(error = %e, "the phone couldn't share its sound");
                    tr!(
                        "PhoneSound",
                        "%1 couldn't ask to share its sound. Check that NectarLink can show notifications on the phone.",
                        name
                    )
                }
            });
            return;
        }
        drop(remote);
        // Nobody answered the phone's prompt: stop waiting.
        tokio::time::sleep(ASK_TIMEOUT).await;
        if clear_current(session, Some(Phase::Asking), Some(Phase::Ready)).is_some() {
            stop_remote(session);
        }
    });
}

fn stop_remote(session: Session) {
    let Some(node) = core_host::node() else { return };
    core_host::spawn(async move {
        let mut remote = REMOTE.lock().await;
        if remote.is_some_and(|s| s.request == session.request) {
            *remote = None;
            node.mirror_stop(session.device, MIRROR_SCREEN).await;
        }
    });
}

/// The mirror window is about to ask `device` for its screen, which brings
/// the sound too: the phone shares one or the other, so this one ends first.
pub fn stop_for_window(device: DeviceId) {
    if let Some(session) = active().filter(|s| s.device == device && s.phase != Phase::Mirroring)
        && let Some(session) = clear_current(session, None, Some(Phase::Mirroring))
        && matches!(session.phase, Phase::Asking | Phase::Playing)
    {
        stop_remote(session);
    }
}

/// The sink for `device`'s sound stream while it plays here without a window.
pub fn sink(device: &DeviceId) -> Option<Arc<dyn MirrorSink>> {
    active()
        .filter(|s| s.device == *device && matches!(s.phase, Phase::Asking | Phase::Playing))
        .map(|session| Arc::new(Sink { session, player: Mutex::new(None) }) as Arc<dyn MirrorSink>)
}

/// Plays the sound stream; there's no picture with it.
struct Sink {
    session: Session,
    player: Mutex<Option<Playback>>,
}

impl MirrorSink for Sink {
    fn config(&self, _config: MirrorConfig) {}

    fn packet(&self, _keyframe: bool, _time_us: u64, _data: Vec<u8>) {}

    fn ended(&self) {}

    fn audio_config(&self, config: MirrorAudioConfig) {
        if !is_current(self.session) {
            return;
        }
        // A new format: a new player (the old one ends when dropped).
        let player = Playback::start(config);
        let playing = player.is_some();
        *self.player.lock().unwrap_or_else(|e| e.into_inner()) = player;
        if playing {
            let playing = {
                let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(s) = active.as_mut().filter(|s| {
                    s.request == self.session.request && matches!(s.phase, Phase::Asking | Phase::Playing)
                }) {
                    s.phase = Phase::Playing;
                    s.calls_armed = !s.for_apps;
                    true
                } else {
                    false
                }
            };
            if playing {
                changed();
                if routes_calls(self.session.device) {
                    crate::call_audio::start(self.session.device, true);
                }
            }
        }
    }

    fn audio(&self, _time_us: u64, data: Vec<u8>) {
        if !active().is_some_and(|s| s.request == self.session.request && s.phase == Phase::Playing) {
            return;
        }
        if let Some(player) = self.player.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            player.push(data);
        }
    }

    fn audio_ended(&self) {
        self.player.lock().unwrap_or_else(|e| e.into_inner()).take();
        // An empty Elevated stream can be followed by the phone's consent
        // prompt. Keep waiting for that fallback until sound plays or times out.
        if clear_current(self.session, Some(Phase::Playing), Some(Phase::Ready)).is_some() {
            let session = self.session;
            core_host::spawn(async move {
                let mut remote = REMOTE.lock().await;
                if remote.is_some_and(|s| s.request == session.request) {
                    *remote = None;
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST: DeviceId = DeviceId([1; 32]);
    const SECOND: DeviceId = DeviceId([2; 32]);

    #[test]
    fn ended_explicit_stream_leaves_calls_armed_until_stopped() {
        let request = Session::ask(SECOND, false, None);
        let playing = Session { phase: Phase::Playing, calls_armed: true, ..request };
        let mut active = Some(playing);
        assert_eq!(
            retire_stream(&mut active, request, Some(Phase::Playing), Some(Phase::Ready)),
            Some(playing)
        );
        assert_eq!(state_for(active, &SECOND), "ready");
        assert_eq!(retire_stream(&mut active, request, Some(Phase::Playing), Some(Phase::Ready)), None);
        retire_stream(&mut active, request, None, None);
        assert_eq!(active, None);

        let app_window = Session { for_apps: true, calls_armed: false, ..playing };
        active = Some(app_window);
        assert_eq!(
            retire_stream(&mut active, app_window, Some(Phase::Playing), Some(Phase::Ready)),
            Some(app_window)
        );
        assert_eq!(active, None);
    }

    #[test]
    fn only_explicit_phone_sound_waits_to_resume_after_link_loss() {
        let playing =
            Session { phase: Phase::Playing, calls_armed: true, ..Session::ask(FIRST, false, None) };
        for phase in [Phase::Playing, Phase::Ready] {
            let previous = Session { phase, ..playing };
            let mut active = Some(previous);
            assert!(!link_lost(&mut active, SECOND));
            assert!(link_lost(&mut active, FIRST));
            let reconnecting = active.unwrap();
            assert_eq!(state_for(active, &FIRST), "reconnecting");
            assert_eq!(retire_stream(&mut active, previous, None, None), None);
            assert!(!link_lost(&mut active, FIRST));
            assert_eq!(retire_stream(&mut active, reconnecting, None, None), Some(reconnecting));
            assert_eq!(active, None);
        }

        let mirroring = Session { phase: Phase::Mirroring, ..playing };
        let mut active = Some(mirroring);
        assert!(!link_lost(&mut active, FIRST));
        assert_eq!(active, Some(mirroring));

        let mut active = Some(Session { for_apps: true, calls_armed: false, ..playing });
        assert!(link_lost(&mut active, FIRST));
        assert_eq!(active, None);
    }

    #[test]
    fn reconnection_keeps_calls_armed_while_media_waits_and_after_it_times_out() {
        for (phase, calls_armed) in [(Phase::Asking, false), (Phase::Playing, true)] {
            let mut active = Some(Session { phase, calls_armed, ..Session::ask(FIRST, false, None) });
            assert!(link_lost(&mut active, FIRST));
            let resumed = Session::ask(FIRST, false, active);
            active = Some(resumed);
            assert_eq!(state_for(active, &FIRST), "asking");
            assert_eq!(resumed.calls_armed, calls_armed);

            retire_stream(&mut active, resumed, Some(Phase::Asking), Some(Phase::Ready));
            assert_eq!(state_for(active, &FIRST), if calls_armed { "ready" } else { "off" });
            assert_eq!(active.is_some_and(|s| s.calls_armed), calls_armed);
        }
    }

    #[test]
    fn timeouts_and_old_streams_cannot_end_playing_or_newer_requests() {
        let old = Session::ask(SECOND, false, None);
        let playing = Session { phase: Phase::Playing, calls_armed: true, ..old };
        let mut active = Some(playing);
        assert_eq!(retire_stream(&mut active, old, Some(Phase::Asking), None), None);
        assert_eq!(active, Some(playing));
        let new = Session::ask(SECOND, false, None);
        active = Some(new);
        assert_eq!(retire_stream(&mut active, old, None, Some(Phase::Ready)), None);
        assert_eq!(active, Some(new));
        assert_eq!(retire_stream(&mut active, new, Some(Phase::Asking), None), Some(new));
    }

    #[test]
    fn empty_elevated_stream_keeps_waiting_for_phone_consent() {
        let request = Session::ask(SECOND, false, None);
        let mut active = Some(request);
        assert_eq!(retire_stream(&mut active, request, Some(Phase::Playing), Some(Phase::Ready)), None);
        assert_eq!(active, Some(request));
        // The fallback capture can start on the same request.
        let playing = Session { phase: Phase::Playing, calls_armed: true, ..request };
        active = Some(playing);
        assert_eq!(state_for(active, &SECOND), "playing");
        assert_eq!(
            retire_stream(&mut active, request, Some(Phase::Playing), Some(Phase::Ready)),
            Some(playing)
        );
    }

    #[test]
    fn requests_target_the_selected_phone_without_falling_back_to_the_first() {
        use crate::state::DeviceView;
        use nectarlink_core::{CapabilityMatrix, ConnectionPath, DeviceInfo, DeviceKind, PowerLevel};

        let mut state = AppState::default();
        for (id, name) in [(FIRST, "Pixel 7a"), (SECOND, "V2443A")] {
            state.devices.push(DeviceView {
                id,
                info: DeviceInfo {
                    name: name.into(),
                    kind: DeviceKind::Phone,
                    os: "android".into(),
                    os_ver: "15".into(),
                    model: None,
                    accent: None,
                    screen: None,
                    install_id: None,
                },
                paired_at: 0,
                link: LinkState::Online { path: ConnectionPath::Lan, rtt_ms: 4 },
                battery: None,
                power: PowerLevel::Basic,
                reconnect: true,
            });
            state.matrices.insert(
                id,
                CapabilityMatrix {
                    device: id,
                    features: [("mirroring.sound", FeatureState::Available)].into(),
                },
            );
        }
        assert_eq!(sharing_phone(&state, SECOND), Some((SECOND, "V2443A".into())));
        state.devices[1].link = LinkState::Offline { last_seen: None };
        assert_eq!(sharing_phone(&state, SECOND), None);
        state.devices[1].link = LinkState::Online { path: ConnectionPath::Lan, rtt_ms: 4 };
        state.matrices.remove(&SECOND);
        assert_eq!(sharing_phone(&state, SECOND), None);
        assert_eq!(sharing_phone(&state, FIRST), Some((FIRST, "Pixel 7a".into())));
    }

    #[test]
    fn the_volume_scales_the_sound_and_leaves_it_alone_at_full() {
        let samples: Vec<u8> =
            [1000i16, -1000, i16::MAX, i16::MIN].iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut full = samples.clone();
        scale(&mut full, 100);
        assert_eq!(full, samples);
        let mut half = samples.clone();
        scale(&mut half, 50);
        let half: Vec<i16> = half.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
        assert_eq!(half, [250, -250, 8192, -8192]);
        let mut silent = samples;
        scale(&mut silent, 0);
        assert!(silent.iter().all(|&b| b == 0));
    }

    #[test]
    fn each_phone_shows_only_its_own_sound_state() {
        for (phase, expected) in
            [(Phase::Asking, "asking"), (Phase::Playing, "playing"), (Phase::Ready, "ready")]
        {
            let first =
                Session { phase, calls_armed: phase != Phase::Asking, ..Session::ask(FIRST, false, None) };
            let second = Session { device: SECOND, ..first };
            assert_eq!(state_for(Some(first), &SECOND), "off");
            assert_eq!(state_for(Some(second), &FIRST), "off");
            assert_eq!(state_for(Some(second), &SECOND), expected);
        }
        assert_eq!(state_for(None, &SECOND), "off");
    }

    #[test]
    fn old_phone_callbacks_and_stop_do_not_clear_the_selected_phone() {
        let first = Session { phase: Phase::Playing, calls_armed: true, ..Session::ask(FIRST, false, None) };
        let second = Session::ask(SECOND, false, None);
        let mut active = Some(second);
        assert_eq!(retire_stream(&mut active, first, None, None), None);
        assert_eq!(active, Some(second));
        let playing = Session { phase: Phase::Playing, calls_armed: true, ..second };
        active = Some(playing);
        assert_eq!(state_for(active, &SECOND), "playing");
        assert_eq!(retire_stream(&mut active, second, None, None), Some(playing));
        assert_eq!(active, None);
    }

    #[test]
    fn switching_back_does_not_accept_callbacks_from_the_old_request() {
        let old = Session::ask(FIRST, false, None);
        let new = Session::ask(FIRST, false, None);
        let mut active = Some(new);
        // A late error, end-of-stream, or timeout from the earlier request.
        assert_eq!(retire_stream(&mut active, old, None, None), None);
        assert_eq!(active, Some(new));
    }
}
