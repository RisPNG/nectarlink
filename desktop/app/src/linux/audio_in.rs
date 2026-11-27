// SPDX-License-Identifier: GPL-3.0-or-later
//! Linux microphone capture for calls, as 16-bit mono PCM.

use std::{
    io::Read,
    os::unix::process::CommandExt,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
    time::Duration,
};

#[derive(Debug)]
pub enum MicError {
    Missing,
    Other(String),
}

impl std::fmt::Display for MicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => f.write_str("no microphone"),
            Self::Other(error) => f.write_str(error),
        }
    }
}

/// Records until dropped.
#[derive(Debug)]
pub struct Recorder {
    child: Child,
    thread: Option<JoinHandle<()>>,
}

impl Recorder {
    pub fn start(
        device: Option<String>,
        rate: u32,
        on_pcm: impl FnMut(Vec<u8>) + Send + 'static,
    ) -> Result<Self, MicError> {
        let devices = super::audio::input_devices();
        let source = devices
            .iter()
            .find(|mic| Some(&mic.id) == device.as_ref())
            .or_else(|| devices.iter().find(|mic| mic.is_default))
            .or_else(|| devices.first())
            .ok_or(MicError::Missing)?;

        let mut pulse = Command::new("parec");
        pulse.args([
            "--raw",
            "--format=s16le",
            &format!("--rate={rate}"),
            "--channels=1",
            "--latency-msec=20",
            "--property=media.role=phone",
            &format!("--device={}", source.id),
        ]);
        let mut pipewire = Command::new("pw-record");
        pipewire.args([
            "--raw",
            "--format=s16",
            &format!("--rate={rate}"),
            "--channels=1",
            "--latency=20ms",
            "--media-role=Communication",
            &format!("--target={}", source.id),
            "-",
        ]);

        let on_pcm = Arc::new(Mutex::new(on_pcm));
        let mut failures = Vec::new();
        for mut command in [pulse, pipewire] {
            let backend = command.get_program().to_string_lossy().into_owned();
            command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
            // SAFETY: prctl(PR_SET_PDEATHSIG, SIGTERM) is async-signal-safe.
            unsafe {
                command.pre_exec(|| {
                    let _ = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    Ok(())
                });
            }
            let child = match command.spawn() {
                Ok(child) => child,
                Err(error) => {
                    failures.push(format!("{backend}: {error}"));
                    continue;
                }
            };
            let mut recorder = Self { child, thread: None };
            let mut stdout = recorder.child.stdout.take().expect("microphone stdout was piped");
            let (ready, started) = mpsc::channel();
            let on_pcm = on_pcm.clone();
            recorder.thread = Some(
                std::thread::Builder::new()
                    .name("pc-mic".into())
                    .spawn(move || {
                        let mut pcm = vec![0; rate as usize / 50 * 2];
                        if stdout.read_exact(&mut pcm).is_err() || ready.send(()).is_err() {
                            return;
                        }
                        let mut on_pcm = on_pcm.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            on_pcm(pcm.clone());
                            if stdout.read_exact(&mut pcm).is_err() {
                                break;
                            }
                        }
                    })
                    .map_err(|error| MicError::Other(error.to_string()))?,
            );
            match started.recv_timeout(Duration::from_secs(2)) {
                Ok(()) => return Ok(recorder),
                Err(error) => failures.push(format!("{backend} produced no microphone audio: {error}")),
            }
        }
        Err(MicError::Other(failures.join("; ")))
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
