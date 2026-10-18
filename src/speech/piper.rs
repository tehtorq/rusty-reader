//! Piper, a natural-sounding neural voice that runs offline (see `scripts/install-piper.sh`).
//!
//! One Piper process stays running per voice and pace: each line of text written to it comes
//! back as a WAV file, whose path it prints once the sentence is ready (in a fraction of the
//! time it takes to say). A second thread plays those files in order through the desktop's
//! sound server, reporting each sentence as it starts and finishes.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{Heard, Voice, VoiceInfo, heard};

pub struct Piper {
    program: PathBuf,
    voices: Vec<(VoiceInfo, PathBuf)>,
    chosen: Option<String>,
    language: String,
    rate: f32,
    synth: Option<Synth>,
    /// Ready sentences, for the player: (id, WAV file).
    ready: Sender<(u64, PathBuf)>,
    /// Sentences with smaller ids were cancelled by `stop`.
    cancelled_below: Arc<Mutex<u64>>,
    speaking: Arc<Mutex<Option<Child>>>,
    last_id: u64,
    out_dir: PathBuf,
}

/// A running Piper process, for one voice at one pace.
struct Synth {
    child: Child,
    stdin: ChildStdin,
    model: PathBuf,
    rate: f32,
    /// Ids of the lines written, oldest first, waiting for their files.
    pending: Arc<Mutex<VecDeque<u64>>>,
}

impl Drop for Synth {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Where `scripts/install-piper.sh` puts Piper.
fn home() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(data.join("rusty-reader/piper"))
}

impl Piper {
    /// Piper, if it is installed with at least one voice, and something can play its audio.
    pub fn find() -> Option<Piper> {
        let home = home()?;
        let program = home.join("piper/piper");
        if !program.is_file() {
            return None;
        }
        let voices = installed_voices(&home.join("voices"));
        if voices.is_empty() {
            return None;
        }
        let player = player()?;
        sweep_old_files();
        let out_dir = std::env::temp_dir().join(format!("rusty-reader-{}", std::process::id()));
        std::fs::create_dir_all(&out_dir).ok()?;

        let (ready, inbox) = channel::<(u64, PathBuf)>();
        let cancelled_below: Arc<Mutex<u64>> = Arc::default();
        let speaking: Arc<Mutex<Option<Child>>> = Arc::default();
        let (cancelled, current) = (cancelled_below.clone(), speaking.clone());
        std::thread::spawn(move || {
            for (id, wav) in inbox {
                if id >= *cancelled.lock().unwrap() {
                    play(player, id, &wav, &cancelled, &current);
                }
                let _ = std::fs::remove_file(&wav);
            }
        });

        Some(Piper {
            program,
            voices,
            chosen: None,
            language: "en".into(),
            rate: 1.0,
            synth: None,
            ready,
            cancelled_below,
            speaking,
            last_id: 0,
            out_dir,
        })
    }

    /// The model to speak with: the one chosen, or the first for the book's language.
    fn model(&self) -> PathBuf {
        let prefix = self.language.split(['-', '_']).next().unwrap_or("en").to_lowercase();
        let chosen = self.chosen.as_ref().and_then(|c| self.voices.iter().find(|(v, _)| &v.id == c));
        let fits = || self.voices.iter().find(|(v, _)| v.language.to_lowercase().starts_with(&prefix));
        chosen.or_else(fits).unwrap_or(&self.voices[0]).1.clone()
    }

    /// The Piper process for the current voice and pace, started (or restarted) as needed.
    fn synth(&mut self) -> Option<&mut Synth> {
        let model = self.model();
        let stale = self.synth.as_ref().is_some_and(|s| s.model != model || s.rate != self.rate);
        if stale || self.synth.as_mut().is_some_and(|s| !matches!(s.child.try_wait(), Ok(None))) {
            self.synth = None;
        }
        if self.synth.is_none() {
            let mut child = Command::new(&self.program)
                .arg("--model")
                .arg(&model)
                .arg("--output_dir")
                .arg(&self.out_dir)
                .arg("--length_scale")
                .arg(format!("{:.3}", 1.0 / self.rate.max(0.25)))
                .arg("--sentence_silence")
                .arg("0.15")
                .arg("--quiet")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok()?;
            let stdin = child.stdin.take()?;
            let stdout = child.stdout.take()?;
            let pending: Arc<Mutex<VecDeque<u64>>> = Arc::default();
            let (waiting, ready) = (pending.clone(), self.ready.clone());
            std::thread::spawn(move || {
                // Piper prints each file's path once it is written, in the order asked for.
                for line in BufReader::new(stdout).lines() {
                    let Ok(path) = line else { break };
                    let Some(id) = waiting.lock().unwrap().pop_front() else { continue };
                    if ready.send((id, PathBuf::from(path.trim()))).is_err() {
                        break;
                    }
                }
                // The process ended: anything still waiting will never come.
                for id in waiting.lock().unwrap().drain(..) {
                    heard(Heard::Failed(id));
                }
            });
            self.synth = Some(Synth { child, stdin, model, rate: self.rate, pending });
        }
        self.synth.as_mut()
    }
}

impl Drop for Piper {
    fn drop(&mut self) {
        self.synth = None;
        let _ = std::fs::remove_dir_all(&self.out_dir);
    }
}

impl Voice for Piper {
    fn speak(&mut self, id: u64, text: &str, _paragraph_end: bool) {
        self.last_id = id;
        let line = text.replace(['\n', '\r'], " ");
        let Some(synth) = self.synth() else {
            heard(Heard::Failed(id));
            return;
        };
        synth.pending.lock().unwrap().push_back(id);
        if writeln!(synth.stdin, "{line}").and_then(|_| synth.stdin.flush()).is_err() {
            synth.pending.lock().unwrap().retain(|&p| p != id);
            heard(Heard::Failed(id));
        }
    }

    fn stop(&mut self) {
        // The narrator's ids only grow, so everything handed over so far is now stale. Piper
        // finishes what it was given (quickly); the player throws the files away.
        *self.cancelled_below.lock().unwrap() = self.last_id + 1;
        if let Some(mut child) = self.speaking.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn set_rate(&mut self, rate: f32) {
        self.rate = rate;
    }

    fn set_language(&mut self, language: &str) {
        self.language = language.to_owned();
    }

    fn set_voice(&mut self, voice: Option<&str>) {
        self.chosen = voice.map(str::to_owned);
    }

    fn current_voice(&mut self) -> Option<String> {
        let model = self.model();
        self.voices.iter().find(|(_, m)| *m == model).map(|(v, _)| v.id.clone())
    }

    fn voices(&mut self) -> Vec<VoiceInfo> {
        self.voices.iter().map(|(v, _)| v.clone()).collect()
    }
}

/// Voices in Piper's folder: `en_GB-alan-medium.onnx` (with its `.onnx.json`) is Alan, a
/// British English voice.
fn installed_voices(dir: &Path) -> Vec<(VoiceInfo, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut voices: Vec<(VoiceInfo, PathBuf)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "onnx") && p.with_extension("onnx.json").is_file())
        .filter_map(|model| {
            let stem = model.file_stem()?.to_str()?.to_owned();
            let mut parts = stem.split('-');
            let locale = parts.next()?;
            let name = parts.next()?;
            let region = locale.split('_').nth(1).unwrap_or(locale);
            let mut label: String = name.replace('_', " ");
            if let Some(first) = label.get(..1) {
                label = first.to_uppercase() + &label[1..];
            }
            let info = VoiceInfo {
                id: stem.clone(),
                language: locale.replace('_', "-"),
                name: format!("{label} ({region})"),
            };
            Some((info, model))
        })
        .collect();
    voices.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    voices
}

/// Remove sentence files left behind by earlier runs that ended without tidying up.
fn sweep_old_files() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.strip_prefix("rusty-reader-")) else { continue };
        let alive = Path::new("/proc").join(pid).exists();
        if !alive && pid.chars().all(|c| c.is_ascii_digit()) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// A command that plays a WAV file and exits when it is done.
fn player() -> Option<&'static str> {
    let path = std::env::var_os("PATH")?;
    ["pw-play", "paplay", "aplay"]
        .into_iter()
        .find(|p| std::env::split_paths(&path).any(|dir| dir.join(p).is_file()))
}

fn play(player: &str, id: u64, wav: &Path, cancelled: &Mutex<u64>, current: &Mutex<Option<Child>>) {
    let Ok(child) = Command::new(player).arg(wav).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
        heard(Heard::Failed(id));
        return;
    };
    *current.lock().unwrap() = Some(child);
    // A stop that came while the player was starting couldn't reach it.
    if id < *cancelled.lock().unwrap() {
        if let Some(mut child) = current.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        return;
    }
    heard(Heard::Started(id));
    loop {
        let mut guard = current.lock().unwrap();
        let Some(child) = guard.as_mut() else { return };
        if !matches!(child.try_wait(), Ok(None)) {
            *guard = None;
            drop(guard);
            if id >= *cancelled.lock().unwrap() {
                heard(Heard::Done(id));
            }
            return;
        }
        drop(guard);
        std::thread::sleep(Duration::from_millis(10));
    }
}
