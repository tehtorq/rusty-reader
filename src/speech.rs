//! Reading aloud.
//!
//! A chapter is split into sentences, which a narrator feeds to the platform's voice one or
//! two ahead, so speech runs on without gaps. The narrator lives on its own thread, so it keeps
//! reading while the screen is off; the page catches up from [`Status`] when it is next drawn.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::book::{Block, BlockKind, Book, Chapter};

// ---------------------------------------------------------------------------------------
// Sentences

/// A sentence of a chapter: its characters `start..end`, and the words to say.
#[derive(Clone, Debug, PartialEq)]
pub struct Sentence {
    pub start: usize,
    pub end: usize,
    pub text: String,
    /// The last in its paragraph (or heading): a reader pauses a little longer after it.
    pub paragraph_end: bool,
}

/// Engines take a few thousand characters at most; very long sentences are said in pieces.
const LONGEST: usize = 1200;

const ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "messrs", "dr", "st", "jr", "sr", "prof", "rev", "capt", "col", "gen", "lt", "sgt",
    "mt", "ft", "vs", "etc", "no", "vol", "ch", "fig", "e.g", "i.e", "cf", "al", "approx",
];

pub fn sentences(chapter: &Chapter) -> Vec<Sentence> {
    let mut out = Vec::new();
    for (block, &base) in chapter.blocks.iter().zip(&chapter.block_starts) {
        if matches!(block.kind, BlockKind::Image(_) | BlockKind::Rule) {
            continue;
        }
        let (chars, spoken) = block_chars(block);
        let first = out.len();
        for range in split(&chars) {
            for range in pieces(&chars, range) {
                let text: String = range.clone().filter(|&i| spoken[i]).map(|i| chars[i]).collect();
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if text.chars().any(char::is_alphanumeric) {
                    let (start, end) = (base + range.start, base + range.end);
                    out.push(Sentence { start, end, text, paragraph_end: false });
                }
            }
        }
        if out.len() > first
            && let Some(last) = out.last_mut()
        {
            last.paragraph_end = true;
        }
    }
    out
}

/// A block's characters, and whether each is to be said: footnote markers are not.
fn block_chars(block: &Block) -> (Vec<char>, Vec<bool>) {
    let mut chars = Vec::new();
    let mut spoken = Vec::new();
    for span in &block.spans {
        for ch in span.text.chars() {
            chars.push(ch);
            spoken.push(!span.style.sup);
        }
    }
    (chars, spoken)
}

/// Sentence ranges within a block's text, trimmed of surrounding spaces.
fn split(chars: &[char]) -> Vec<std::ops::Range<usize>> {
    let closing = |c: char| matches!(c, '"' | '\'' | '”' | '’' | ')' | ']' | '»');
    let opening = |c: char| matches!(c, '"' | '\'' | '“' | '‘' | '(' | '[' | '«');
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < chars.len() {
        if !matches!(chars[i], '.' | '!' | '?' | '…') {
            i += 1;
            continue;
        }
        let stop = i;
        let mut j = i + 1;
        while j < chars.len() && (matches!(chars[j], '.' | '!' | '?' | '…') || closing(chars[j])) {
            j += 1;
        }
        i = j;
        let next = chars[j..].iter().position(|c| !c.is_whitespace()).map(|k| chars[j + k]);
        let ends = match next {
            None => true,
            Some(c) => {
                j < chars.len()
                    && chars[j].is_whitespace()
                    && (c.is_uppercase() || c.is_numeric() || opening(c))
                    && !(chars[stop] == '.' && abbreviation(&chars[start..stop]))
            }
        };
        if ends {
            out.push(start..j);
            start = j;
        }
    }
    if start < chars.len() {
        out.push(start..chars.len());
    }
    out.into_iter()
        .filter_map(|r| {
            let s = r.start + chars[r.clone()].iter().position(|c| !c.is_whitespace())?;
            let e = r.end - chars[r].iter().rev().position(|c| !c.is_whitespace())?;
            Some(s..e)
        })
        .collect()
}

/// Whether the word just before a full stop is a title or an initial, which a sentence
/// carries on past.
fn abbreviation(before: &[char]) -> bool {
    let word: String = before
        .iter()
        .rev()
        .take_while(|c| !c.is_whitespace())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let word = word.trim_start_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    let letters = word.chars().filter(|c| c.is_alphabetic()).count();
    (letters == 1 && word.chars().all(|c| c.is_alphabetic() || c == '.'))
        || ABBREVIATIONS.contains(&word.as_str())
}

/// Break an overlong sentence at commas (or spaces, failing that).
fn pieces(chars: &[char], range: std::ops::Range<usize>) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut start = range.start;
    while range.end - start > LONGEST {
        let window = start..start + LONGEST;
        let cut = [',', ';', ':', ' ']
            .iter()
            .find_map(|&mark| window.clone().rev().find(|&k| chars[k] == mark && k > start + LONGEST / 3))
            .map_or(window.end, |k| k + 1);
        out.push(start..cut);
        start = cut;
        while start < range.end && chars[start].is_whitespace() {
            start += 1;
        }
    }
    out.push(start..range.end);
    out
}

// ---------------------------------------------------------------------------------------
// Voices

/// What a platform's speech engine can do. Calls come from the narrator's thread; the engine
/// reports progress through [`heard`].
pub trait Voice: Send {
    /// Say this after anything already queued; a paragraph's last sentence is followed by a
    /// longer pause, where the voice can manage one.
    fn speak(&mut self, id: u64, text: &str, paragraph_end: bool);
    /// Fall silent and forget the queue.
    fn stop(&mut self);
    /// 1.0 is the voice's normal pace.
    fn set_rate(&mut self, rate: f32);
    /// A BCP 47 language tag, such as "en-GB".
    fn set_language(&mut self, language: &str);
    /// One of [`Voice::voices`], or `None` for the engine's choice.
    fn set_voice(&mut self, voice: Option<&str>);
    fn voices(&mut self) -> Vec<VoiceInfo>;
    /// The voice speaking now, as one of [`Voice::voices`].
    fn current_voice(&mut self) -> Option<String> {
        None
    }
    /// Speech engines to choose between, the one in use first (only Android has several).
    fn engines(&mut self) -> Vec<Engine> {
        Vec::new()
    }
    /// One of [`Voice::engines`], or `None` for the platform's choice.
    fn set_engine(&mut self, _engine: Option<&str>) {}
    /// The book being read, for lock-screen controls.
    fn now_playing(&mut self, _state: State, _title: &str, _author: Option<&str>) {}
}

#[derive(Clone, Debug, PartialEq)]
pub struct VoiceInfo {
    pub id: String,
    /// A BCP 47 language tag.
    pub language: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Engine {
    pub id: String,
    pub name: String,
}

/// How the reading should sound.
#[derive(Clone, Debug, PartialEq)]
pub struct VoicePrefs {
    /// 1.0 is the voice's own pace.
    pub rate: f32,
    pub voice: Option<String>,
    pub engine: Option<String>,
}

/// Progress reports from a voice.
#[derive(Clone, Copy, Debug)]
pub enum Heard {
    Started(u64),
    Done(u64),
    Failed(u64),
}

/// Requests from outside the app: headphone buttons, the lock screen, a phone call.
#[derive(Clone, Copy, Debug)]
pub enum Control {
    Play,
    Pause,
    Toggle,
    Stop,
    Next,
    Previous,
}

static INBOX: Mutex<Option<Sender<Msg>>> = Mutex::new(None);

/// Engines call this as utterances start and finish.
pub fn heard(event: Heard) {
    if let Some(tx) = INBOX.lock().ok().and_then(|tx| tx.clone()) {
        let _ = tx.send(Msg::Heard(event));
    }
}

/// Voices that download themselves call this: `fraction` runs from 0 to 1, or is negative if
/// the download failed.
pub fn download_progress(voice: &str, fraction: f32) {
    if let Some(tx) = INBOX.lock().ok().and_then(|tx| tx.clone()) {
        let _ = tx.send(Msg::Download(voice.to_owned(), fraction));
    }
}

/// Voices call this when something went wrong that the reader should hear about.
pub fn failure(message: &str) {
    if let Some(tx) = INBOX.lock().ok().and_then(|tx| tx.clone()) {
        let _ = tx.send(Msg::Failure(message.to_owned()));
    }
}

/// Media buttons and the like call this.
pub fn control(control: Control) {
    if let Some(tx) = INBOX.lock().ok().and_then(|tx| tx.clone()) {
        let _ = tx.send(Msg::Control(control));
    }
}

// ---------------------------------------------------------------------------------------
// The narrator

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Idle,
    Playing,
    Paused,
}

/// Where the reading is: the sentence `start..end` of `chapter`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spot {
    pub chapter: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub state: State,
    pub at: Option<Spot>,
    /// Voices for the book's language, once the engine has said.
    pub voices: Vec<VoiceInfo>,
    /// Speech engines, the one in use first.
    pub engines: Vec<Engine>,
    /// The voice speaking now.
    pub voice: Option<String>,
    /// A voice being downloaded, and how far it has got (negative: it failed).
    pub download: Option<(String, f32)>,
    /// What last went wrong, and a count that goes up each time something does.
    pub error: Option<(String, u32)>,
}

enum Msg {
    Play {
        book: Arc<Book>,
        path: PathBuf,
        chapter: usize,
        offset: usize,
    },
    Control(Control),
    Prefs(VoicePrefs),
    Heard(Heard),
    Download(String, f32),
    Failure(String),
    /// Stop, and say so once silent.
    Halt(Sender<()>),
}

/// The app's handle on the narrator. The engine starts the first time it is asked to read.
#[derive(Default)]
pub struct Narrator {
    tx: Option<Sender<Msg>>,
    status: Arc<Mutex<Status>>,
}

impl Narrator {
    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Read from the sentence at `offset` on.
    pub fn play(
        &mut self,
        ctx: &egui::Context,
        book: Arc<Book>,
        path: PathBuf,
        chapter: usize,
        offset: usize,
        prefs: VoicePrefs,
    ) {
        if self.tx.is_none() {
            let (tx, rx) = channel();
            if let Ok(mut inbox) = INBOX.lock() {
                *inbox = Some(tx.clone());
            }
            let status = self.status.clone();
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name("narrator".into())
                .spawn(move || Worker::new(status, ctx, prefs).run(rx))
                .ok();
            self.tx = Some(tx);
        }
        self.send(Msg::Play { book, path, chapter, offset });
    }

    pub fn control(&self, control: Control) {
        self.send(Msg::Control(control));
    }

    /// Stop, and wait (briefly) until the voice has fallen silent: for when the app closes,
    /// which would otherwise leave the sentence being said playing on.
    pub fn halt(&self) {
        let (tx, rx) = channel();
        if self.tx.is_some() {
            self.send(Msg::Halt(tx));
            let _ = rx.recv_timeout(std::time::Duration::from_millis(500));
        }
    }

    pub fn set_prefs(&self, prefs: VoicePrefs) {
        self.send(Msg::Prefs(prefs));
    }

    fn send(&self, msg: Msg) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(msg);
        }
    }
}

/// Utterances kept with the engine, the one being said included: enough that the next is
/// always ready the moment one ends.
const AHEAD: usize = 3;

struct Worker {
    voice: Box<dyn Voice>,
    status: Arc<Mutex<Status>>,
    ctx: egui::Context,
    book: Option<Arc<Book>>,
    language: Option<String>,
    sentences: HashMap<usize, Arc<Vec<Sentence>>>,
    /// With the engine, in order: (id, chapter, sentence index).
    queued: VecDeque<(u64, usize, usize)>,
    /// The next sentence to hand over.
    next: (usize, usize),
    next_id: u64,
    state: State,
    prefs: VoicePrefs,
    /// The book's file, and the last sentence begun in it: kept in the reading position, since
    /// with the screen off the page (which usually does that) isn't there to follow.
    path: Option<PathBuf>,
    place: Option<Spot>,
    saved: Option<Instant>,
}

/// How often to note the place while reading on.
const SAVE_EVERY: Duration = Duration::from_secs(20);

impl Worker {
    fn new(status: Arc<Mutex<Status>>, ctx: egui::Context, prefs: VoicePrefs) -> Worker {
        let mut engine = engine();
        engine.set_engine(prefs.engine.as_deref());
        engine.set_rate(prefs.rate);
        engine.set_voice(prefs.voice.as_deref());
        Worker {
            voice: engine,
            status,
            ctx,
            book: None,
            language: None,
            sentences: HashMap::new(),
            queued: VecDeque::new(),
            next: (0, 0),
            next_id: 1,
            state: State::Idle,
            prefs,
            path: None,
            place: None,
            saved: None,
        }
    }

    fn run(mut self, rx: Receiver<Msg>) {
        for msg in rx {
            match msg {
                Msg::Play { book, path, chapter, offset } => {
                    self.path = Some(path);
                    self.play(book, chapter, offset);
                }
                Msg::Control(control) => self.control(control),
                Msg::Halt(done) => {
                    self.control(Control::Stop);
                    let _ = done.send(());
                }
                Msg::Prefs(prefs) => self.set_prefs(prefs),
                Msg::Failure(message) => {
                    self.publish(|s| {
                        let count = s.error.as_ref().map_or(0, |e| e.1) + 1;
                        s.error = Some((message, count));
                    });
                }
                Msg::Download(voice, fraction) => {
                    self.publish(|s| s.download = (fraction < 1.0).then_some((voice, fraction)));
                    if fraction >= 1.0 {
                        // Downloaded: its name no longer needs to say how big it is.
                        self.list_voices();
                    }
                }
                Msg::Heard(Heard::Started(id)) => {
                    if let Some(&(_, chapter, index)) = self.queued.iter().find(|q| q.0 == id) {
                        let sentence = &self.sentences(chapter)[index];
                        let at = Spot { chapter, start: sentence.start, end: sentence.end };
                        self.publish(|s| s.at = Some(at));
                        self.place = Some(at);
                        if self.saved.is_none_or(|t| t.elapsed() > SAVE_EVERY) {
                            self.save_place();
                        }
                    }
                }
                Msg::Heard(Heard::Failed(id)) if self.queued.iter().any(|q| q.0 == id) => {
                    // No voice, or the engine gave up: stop rather than skip through the book.
                    self.control(Control::Stop);
                }
                Msg::Heard(Heard::Done(id) | Heard::Failed(id)) => {
                    if let Some(i) = self.queued.iter().position(|q| q.0 == id) {
                        self.queued.drain(..=i);
                        self.fill();
                        if self.queued.is_empty() && self.state == State::Playing {
                            // The end of the book.
                            self.set_state(State::Idle);
                            self.publish(|s| s.at = None);
                        }
                    }
                }
            }
        }
    }

    fn play(&mut self, book: Arc<Book>, chapter: usize, offset: usize) {
        if !self.book.as_ref().is_some_and(|b| Arc::ptr_eq(b, &book)) {
            self.sentences.clear();
            let language = book.language.clone().unwrap_or_else(|| "en".into());
            if self.language.as_ref() != Some(&language) {
                self.voice.set_language(&language);
                self.language = Some(language);
                self.list_voices();
            }
            self.book = Some(book);
        }
        self.voice.stop();
        self.queued.clear();
        let index = self.sentences(chapter).partition_point(|s| s.end <= offset);
        self.next = (chapter, index);
        self.set_state(State::Playing);
        self.fill();
    }

    /// Tell the app which engines and voices there are (for the book's language).
    fn list_voices(&mut self) {
        let prefix =
            self.language.as_deref().unwrap_or("en").split(['-', '_']).next().unwrap_or("en").to_lowercase();
        let voices: Vec<VoiceInfo> = self
            .voice
            .voices()
            .into_iter()
            .filter(|v| v.language.to_lowercase().starts_with(&prefix))
            .collect();
        let engines = self.voice.engines();
        let current = self.voice.current_voice();
        self.publish(|s| {
            s.voices = voices;
            s.engines = engines;
            s.voice = current;
        });
    }

    fn set_prefs(&mut self, prefs: VoicePrefs) {
        if prefs == self.prefs {
            return;
        }
        let playing = self.state == State::Playing;
        if playing {
            self.rewind();
        }
        if prefs.engine != self.prefs.engine {
            self.voice.set_engine(prefs.engine.as_deref());
            // A new engine starts afresh: it needs telling everything again.
            if let Some(language) = self.language.clone() {
                self.voice.set_language(&language);
            }
            self.voice.set_rate(prefs.rate);
            self.voice.set_voice(prefs.voice.as_deref());
            self.list_voices();
        } else {
            if prefs.rate != self.prefs.rate {
                self.voice.set_rate(prefs.rate);
            }
            if prefs.voice != self.prefs.voice {
                self.voice.set_voice(prefs.voice.as_deref());
                let current = self.voice.current_voice();
                self.publish(|s| s.voice = current);
            }
        }
        self.prefs = prefs;
        if playing {
            self.fill();
        }
    }

    fn control(&mut self, control: Control) {
        match (control, self.state) {
            (Control::Pause | Control::Toggle, State::Playing) => {
                self.rewind();
                self.set_state(State::Paused);
            }
            (Control::Play | Control::Toggle, State::Paused) => {
                self.set_state(State::Playing);
                self.fill();
            }
            (Control::Stop, _) => {
                self.voice.stop();
                self.queued.clear();
                self.set_state(State::Idle);
                self.publish(|s| s.at = None);
            }
            (Control::Next | Control::Previous, State::Playing | State::Paused) => {
                let (chapter, index) = self.queued.front().map_or(self.next, |q| (q.1, q.2));
                let index =
                    if matches!(control, Control::Next) { index + 1 } else { index.saturating_sub(1) };
                self.voice.stop();
                self.queued.clear();
                self.next = (chapter, index);
                if let Some(sentence) = self.sentences(chapter).get(index) {
                    let at = Spot { chapter, start: sentence.start, end: sentence.end };
                    self.publish(|s| s.at = Some(at));
                }
                if self.state == State::Playing {
                    self.fill();
                }
            }
            _ => {}
        }
    }

    /// Silence, ready to carry on from the start of the sentence being said.
    fn rewind(&mut self) {
        self.voice.stop();
        if let Some(&(_, chapter, index)) = self.queued.front() {
            self.next = (chapter, index);
        }
        self.queued.clear();
    }

    /// Keep the engine a sentence or two ahead.
    fn fill(&mut self) {
        let Some(book) = self.book.clone() else { return };
        while self.state == State::Playing && self.queued.len() < AHEAD {
            let (mut chapter, mut index) = self.next;
            while chapter < book.chapters.len() && index >= self.sentences(chapter).len() {
                chapter += 1;
                index = 0;
            }
            if chapter >= book.chapters.len() {
                return;
            }
            let id = self.next_id;
            self.next_id += 1;
            let sentence = &self.sentences(chapter)[index];
            let (text, paragraph_end) = (sentence.text.clone(), sentence.paragraph_end);
            self.voice.speak(id, &text, paragraph_end);
            self.queued.push_back((id, chapter, index));
            self.next = (chapter, index + 1);
            // Only the chapters being read stay split up.
            self.sentences.retain(|&c, _| c + 1 >= chapter && c <= chapter + 1);
        }
    }

    fn sentences(&mut self, chapter: usize) -> Arc<Vec<Sentence>> {
        let Some(book) = &self.book else { return Arc::default() };
        let Some(source) = book.chapters.get(chapter) else { return Arc::default() };
        self.sentences.entry(chapter).or_insert_with(|| Arc::new(sentences(source))).clone()
    }

    /// Write the place reached into the reading position.
    fn save_place(&mut self) {
        let (Some(path), Some(place), Some(book)) = (&self.path, self.place, &self.book) else { return };
        let progress = book.progress(place.chapter, place.start);
        crate::store::Store::remember_place(path, place.chapter, place.start, progress);
        self.saved = Some(Instant::now());
    }

    fn set_state(&mut self, state: State) {
        if state != State::Playing {
            self.save_place();
        }
        self.state = state;
        if let Some(book) = &self.book {
            self.voice.now_playing(state, &book.title, book.author.as_deref());
        }
        self.publish(|s| s.state = state);
    }

    fn publish(&self, change: impl FnOnce(&mut Status)) {
        if let Ok(mut status) = self.status.lock() {
            change(&mut status);
        }
        self.ctx.request_repaint();
    }
}

fn engine() -> Box<dyn Voice> {
    #[cfg(target_os = "android")]
    {
        Box::new(crate::platform::android::SystemVoice)
    }
    #[cfg(not(target_os = "android"))]
    {
        // Piper sounds far better, where it has been installed; speech-dispatcher otherwise.
        let silent = std::env::var_os("RUSTY_READER_SILENT_VOICE").is_some();
        match piper::Piper::find().filter(|_| !silent) {
            Some(piper) => Box::new(piper),
            None => Box::new(desktop::SpeechDispatcher::new()),
        }
    }
}

#[cfg(not(target_os = "android"))]
mod piper;

#[cfg(not(target_os = "android"))]
mod desktop {
    //! Linux's speech-dispatcher, through its `spd-say` command: one process per sentence,
    //! which `-w` keeps running until the sentence has been said.

    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc::{Sender, channel};
    use std::sync::{Arc, Mutex};

    use super::{Heard, Voice, VoiceInfo, heard};

    pub struct SpeechDispatcher {
        queue: Sender<(u64, String)>,
        settings: Arc<Mutex<Settings>>,
        speaking: Arc<Mutex<Option<Child>>>,
        /// Sentences with smaller ids were cancelled by `stop`.
        cancelled_below: Arc<Mutex<u64>>,
        last_id: u64,
        silent: bool,
    }

    #[derive(Clone, Default)]
    struct Settings {
        rate: f32,
        language: String,
        voice: Option<String>,
    }

    impl SpeechDispatcher {
        pub fn new() -> SpeechDispatcher {
            let (queue, inbox) = channel::<(u64, String)>();
            let settings = Arc::new(Mutex::new(Settings { rate: 1.0, ..Default::default() }));
            let speaking: Arc<Mutex<Option<Child>>> = Arc::default();
            let cancelled_below: Arc<Mutex<u64>> = Arc::default();
            let (s, sp, g) = (settings.clone(), speaking.clone(), cancelled_below.clone());
            // RUSTY_READER_SILENT_VOICE=0.004 "reads" at 4 ms a character, without a sound.
            let silent: Option<f32> =
                std::env::var("RUSTY_READER_SILENT_VOICE").ok().map(|v| v.parse().unwrap_or(0.04));
            std::thread::spawn(move || {
                for (id, text) in inbox {
                    if id < *g.lock().unwrap() {
                        continue;
                    }
                    let settings = s.lock().unwrap().clone();
                    if silent.is_some() {
                        // For testing: "say" each sentence in a time to suit its length.
                        heard(Heard::Started(id));
                        let secs = silent.unwrap_or(0.04) * text.len() as f32 / settings.rate.max(0.1);
                        let end = std::time::Instant::now() + std::time::Duration::from_secs_f32(secs);
                        while std::time::Instant::now() < end && id >= *g.lock().unwrap() {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        if id >= *g.lock().unwrap() {
                            heard(Heard::Done(id));
                        }
                        continue;
                    }
                    // spd-say's rate runs from -100 to 100; double speed is about 60.
                    let rate = ((settings.rate.log2() * 60.0).round() as i32).clamp(-100, 100);
                    let mut command = Command::new("spd-say");
                    command.arg("-w").arg("-r").arg(rate.to_string());
                    if !settings.language.is_empty() {
                        command.arg("-l").arg(&settings.language);
                    }
                    if let Some(voice) = &settings.voice {
                        command.arg("-y").arg(voice);
                    }
                    command.arg("--").arg(&text).stdout(Stdio::null()).stderr(Stdio::null());
                    let Ok(child) = command.spawn() else {
                        heard(Heard::Failed(id));
                        continue;
                    };
                    *sp.lock().unwrap() = Some(child);
                    // A stop that came while the process was starting couldn't reach it.
                    if id < *g.lock().unwrap() {
                        if let Some(mut child) = sp.lock().unwrap().take() {
                            let _ = child.kill();
                            let _ = child.wait();
                        }
                        continue;
                    }
                    heard(Heard::Started(id));
                    loop {
                        let mut guard = sp.lock().unwrap();
                        let Some(child) = guard.as_mut() else { break };
                        match child.try_wait() {
                            Ok(Some(_)) | Err(_) => {
                                *guard = None;
                                drop(guard);
                                if id >= *g.lock().unwrap() {
                                    heard(Heard::Done(id));
                                }
                                break;
                            }
                            Ok(None) => {}
                        }
                        drop(guard);
                        std::thread::sleep(std::time::Duration::from_millis(15));
                    }
                }
            });
            SpeechDispatcher {
                queue,
                settings,
                speaking,
                cancelled_below,
                last_id: 0,
                silent: silent.is_some(),
            }
        }
    }

    impl Voice for SpeechDispatcher {
        fn speak(&mut self, id: u64, text: &str, _paragraph_end: bool) {
            self.last_id = id;
            let _ = self.queue.send((id, text.to_owned()));
        }

        fn stop(&mut self) {
            // The narrator's ids only grow, so everything handed over so far is now stale.
            *self.cancelled_below.lock().unwrap() = self.last_id + 1;
            if let Some(mut child) = self.speaking.lock().unwrap().take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            if !self.silent {
                let _ =
                    Command::new("spd-say").arg("-C").stdout(Stdio::null()).stderr(Stdio::null()).status();
            }
        }

        fn set_rate(&mut self, rate: f32) {
            self.settings.lock().unwrap().rate = rate;
        }

        fn set_language(&mut self, language: &str) {
            self.settings.lock().unwrap().language = language.to_owned();
        }

        fn set_voice(&mut self, voice: Option<&str>) {
            self.settings.lock().unwrap().voice = voice.map(str::to_owned);
        }

        fn voices(&mut self) -> Vec<VoiceInfo> {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        split(&chars).into_iter().map(|r| chars[r].iter().collect()).collect()
    }

    #[test]
    fn sentences_end_where_a_reader_would_pause() {
        assert_eq!(
            said("Mr. Thom Merrilin bowed. “Who are you?” she asked. He did not say! Then… nothing."),
            ["Mr. Thom Merrilin bowed.", "“Who are you?” she asked.", "He did not say!", "Then… nothing."]
        );
        assert_eq!(
            said("It cost J. R. R. Tolkien 3.5 pounds. Fine."),
            ["It cost J. R. R. Tolkien 3.5 pounds.", "Fine."]
        );
        assert_eq!(said("  No stop at the end  "), ["No stop at the end"]);
    }

    #[test]
    fn long_sentences_break_at_commas() {
        let text = "word, ".repeat(400);
        let chars: Vec<char> = text.chars().collect();
        let parts = pieces(&chars, 0..chars.len());
        assert!(parts.len() > 1);
        assert!(parts.iter().all(|r| r.len() <= LONGEST));
        assert_eq!(parts.last().unwrap().end, chars.len());
    }
}
