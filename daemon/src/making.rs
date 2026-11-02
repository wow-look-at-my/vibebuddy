//! Making a Character (ADR-0020, docs/characters.md): the lines the user wrote, spoken in a voice from
//! `characters/voices.tsv`, made into audio pools the App packs with a look.
//!
//! `POST /v1/characters` starts one job at a time on its own thread. Each line is synthesized as 16 kHz PCM with the
//! user's own ElevenLabs key. A voice of the user's own ([`crate::own_voices`]) takes the base preset's language. Then,
//! as `tools/make-character.sh` does with ffmpeg, it is trimmed of leading and trailing silence and normalized to a -1
//! trimmed of leading and trailing silence and normalized to a -1 dBFS peak. Everything lands in
//! `config_dir()/characters/<id>/`, laid out as `make-character.sh` lays out `characters/<id>/`:
//!
//! - `audio/<occasion>/<prefix>-<hash>.pcm` (and `.txt`), and `audio-address/<form>/...` for the lines with
//!   `{address}`, said once with each form of the voice's language from `characters/addresses.tsv`;
//! - `pack.bin` and `address/<form>.bin`, Character packs exactly as `tools/character_pack.py` builds them (16 kHz IMA
//!   ADPCM, no look), written once every line is made: the App reads them as it reads a preset's;
//! - `manifest.json`: the name, base, voice and lines asked for.
//!
//! A line already made is never made again. A refusal (the provider refusing the own key, or no key, `own_key`)
//! stops the job, keeps the lines made, and starting the same Character again (its `id`) picks up where it
//! (its `id`) picks up where it stopped. Progress, the state and why it stopped are `making` in `/v1/status`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha1::{Digest, Sha1};
use tokio::sync::watch;
use tracing::{info, warn};
use vibebuddy_firmware_core::adpcm::Encoder;
use vibebuddy_firmware_core::audio::Occasion;

use crate::own_voices::{OwnVoice, OwnVoices};
use crate::synthesis::{ELEVENLABS_MODEL, MAX_TEXT};
use crate::voice_keys::{Provider, VoiceKeys};

const VOICES: &str = include_str!("../../characters/voices.tsv");
const ADDRESSES: &str = include_str!("../../characters/addresses.tsv");
pub const MANIFEST: &str = "manifest.json";
/// The most lines a pack with a look has room for, which is what the App makes of these.
const MAX_LINES: usize = (1008 - 128) / 8;
/// No line needs more: about a minute of 16 kHz PCM.
const MAX_AUDIO: u64 = 2 * 1024 * 1024;
const SAMPLE_RATE: u32 = 16_000;

/// A voice a made Character can speak with: a row of `characters/voices.tsv`, or one of the user's own.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Voice {
    pub id: String,
    pub provider: String,
    /// The provider's id for it.
    pub voice: String,
    /// Empty for one of the user's own, which speaks the base preset's language.
    pub language: String,
    pub gender: String,
    pub name: String,
    /// In the user's own account.
    pub own: bool,
}

impl From<&OwnVoice> for Voice {
    fn from(own: &OwnVoice) -> Self {
        Self {
            id: own.id.clone(),
            provider: own.provider.clone(),
            voice: own.voice.clone(),
            language: String::new(),
            gender: String::new(),
            name: own.name.clone(),
            own: true,
        }
    }
}

pub fn voices() -> Vec<Voice> {
    rows(VOICES)
        .filter_map(|row| match row[..] {
            [id, provider, voice, language, gender, name] => Some(Voice {
                id: id.to_owned(),
                provider: provider.to_owned(),
                voice: voice.to_owned(),
                language: language.to_owned(),
                gender: gender.to_owned(),
                name: name.to_owned(),
                own: false,
            }),
            _ => None,
        })
        .collect()
}

/// The language a preset speaks, from its voice in `voices.tsv`.
fn preset_language(id: &str) -> String {
    voices()
        .into_iter()
        .find(|voice| voice.id == id)
        .map(|voice| voice.language)
        .unwrap_or_default()
}

/// Each preset's lines (`characters/<id>/lines.tsv`), which a made Character starts from in the App.
const PRESET_LINES: &[(&str, &str)] = &[
    ("ada", include_str!("../../characters/ada/lines.tsv")),
    ("dave", include_str!("../../characters/dave/lines.tsv")),
    ("hank", include_str!("../../characters/hank/lines.tsv")),
    ("kai", include_str!("../../characters/kai/lines.tsv")),
    ("luna", include_str!("../../characters/luna/lines.tsv")),
    ("mei", include_str!("../../characters/mei/lines.tsv")),
    (
        "wanwanxiaohe",
        include_str!("../../characters/wanwanxiaohe/lines.tsv"),
    ),
];

/// The lines of preset `id`, in `lines.tsv` order; None for anything not a preset.
pub fn preset_lines(id: &str) -> Option<Vec<Line>> {
    let (_, tsv) = PRESET_LINES.iter().find(|(preset, _)| *preset == id)?;
    Some(
        rows(tsv)
            .filter_map(|row| match row[..] {
                [occasion, text] => Some(Line {
                    occasion: occasion.to_owned(),
                    text: text.to_owned(),
                }),
                _ => None,
            })
            .collect(),
    )
}

/// A TSV's rows, without blank ones and `#` comments.
fn rows(tsv: &str) -> impl Iterator<Item = Vec<&str>> {
    tsv.lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| line.split('\t').collect())
}

/// The forms of address of `language`: id and the words spoken.
fn forms(language: &str) -> Vec<(&'static str, &'static str)> {
    rows(ADDRESSES)
        .filter_map(|row| match row[..] {
            [lang, form, words] if lang == language => Some((form, words)),
            _ => None,
        })
        .collect()
}

/// A line said with no form of address: `{address}` and the comma or space that joins it go, as in
/// `tools/make-character.sh`.
pub fn without_address(text: &str) -> String {
    static PATTERNS: std::sync::OnceLock<[(Regex, &str); 4]> = std::sync::OnceLock::new();
    // make-character.sh's lookaheads, as a captured character put back.
    let patterns = PATTERNS.get_or_init(|| {
        [
            (
                Regex::new(r"[，,]\s*\{address\}([。！？.!?，,]|$)").unwrap(),
                "$1",
            ),
            (Regex::new(r"\s\{address\}([,.!?])").unwrap(), "$1"),
            (Regex::new(r"\{address\}[，,]\s*").unwrap(), ""),
            (Regex::new(r"\s?\{address\}").unwrap(), ""),
        ]
    });
    patterns
        .iter()
        .fold(text.to_owned(), |text, (pattern, with)| {
            pattern.replace_all(&text, *with).into_owned()
        })
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Line {
    pub occasion: String,
    pub text: String,
}

/// `POST /v1/characters`, and the manifest without the `id`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Request {
    /// A Character made before, to make again; None for a new one.
    #[serde(default, skip_serializing)]
    pub id: Option<String>,
    pub name: String,
    /// The preset it started from.
    pub base: String,
    /// An id from `characters/voices.tsv`, or of one of the user's own voices.
    pub voice: String,
    pub lines: Vec<Line>,
}

/// One piece of audio to make: where it goes under the Character's directory, and what it says.
#[derive(Debug, PartialEq)]
struct Piece {
    path: PathBuf,
    text: String,
}

/// What `make-character.sh` synthesizes for these lines, named as it names them: a line with `{address}` once without a
/// form into `audio/` and once with each form into `audio-address/<form>/`, prefixed `0-<row>` so they come first in
/// their pool in row order; any other line once into `audio/`, prefixed `1`.
fn pieces(lines: &[Line], forms: &[(&str, &str)]) -> Vec<Piece> {
    let piece = |into: PathBuf, occasion: &str, prefix: &str, text: String| {
        let hash: String = Sha1::digest(text.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Piece {
            path: into
                .join(occasion)
                .join(format!("{prefix}-{}.pcm", &hash[..10])),
            text,
        }
    };
    let mut pieces = Vec::new();
    for (row, line) in lines.iter().enumerate() {
        if line.text.contains("{address}") {
            let prefix = format!("0-{:03}", row + 1);
            pieces.push(piece(
                "audio".into(),
                &line.occasion,
                &prefix,
                without_address(&line.text),
            ));
            for (form, words) in forms {
                let into = Path::new("audio-address").join(form);
                pieces.push(piece(
                    into,
                    &line.occasion,
                    &prefix,
                    line.text.replace("{address}", words),
                ));
            }
        } else {
            pieces.push(piece(
                "audio".into(),
                &line.occasion,
                "1",
                line.text.clone(),
            ));
        }
    }
    // The same text twice in a pool is one line, as one file.
    let mut seen = std::collections::HashSet::new();
    pieces.retain(|piece| seen.insert(piece.path.clone()));
    pieces
}

/// `making` in the status: the job running, or the last one.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MakingStatus {
    pub id: String,
    pub name: String,
    /// `making`, `done` or `failed`.
    pub state: &'static str,
    /// Pieces of audio made, of all there are: each line, and each form of address of those with one.
    pub done: usize,
    pub total: usize,
    /// Why it stopped, for the App to explain: `own_key` or `internal`; None for
    /// anything else, which `error` describes.
    pub reason: Option<&'static str>,
    pub error: Option<String>,
}

/// A Character made here, in `GET /v1/characters`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MadeCharacter {
    pub id: String,
    pub name: String,
    pub base: String,
    pub voice: String,
    pub lines: usize,
    /// Its packs are made, for the lines it has now.
    pub complete: bool,
}

/// Where the Characters made here are, for [`voice_of`]; set once at start, never in tests.
pub static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
/// The user's own voices and keys, for [`voice_of`]; set once at start, never in tests.
pub static OWN: std::sync::OnceLock<(Arc<OwnVoices>, Arc<VoiceKeys>)> = std::sync::OnceLock::new();

/// The name (`<provider>:<voice>`) of the voice of `id`, a Character made in
/// `dir`.
pub fn voice_of(dir: &Path, id: &str) -> Option<String> {
    voice_with(
        dir,
        id,
        OWN.get()
            .map(|(voices, keys)| (voices.as_ref(), keys.as_ref())),
    )
}

fn voice_with(dir: &Path, id: &str, own: Option<(&OwnVoices, &VoiceKeys)>) -> Option<String> {
    if !valid_id(id) {
        return None;
    }
    let manifest: Request =
        serde_json::from_str(&std::fs::read_to_string(dir.join(id).join(MANIFEST)).ok()?).ok()?;
    if let Some(voice) = voices()
        .into_iter()
        .find(|voice| voice.id == manifest.voice)
    {
        return Some(format!("{}:{}", voice.provider, voice.voice));
    }
    let usable = own.and_then(|(voices, keys)| {
        let voice = voices.get(&manifest.voice)?;
        Provider::named(&voice.provider)?;
        let key = keys.status().elevenlabs;
        (key.set && !key.refused).then(|| format!("{}:{}", voice.provider, voice.voice))
    });
    Some(usable.unwrap_or_else(|| crate::synthesis::voice_for(Some(&manifest.base))))
}

/// A voice the user's own ElevenLabs key can speak, from that account's voice library: its id, name and kind
/// (`premade`, `cloned`, `generated`, `professional`).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProviderVoice {
    #[serde(rename(deserialize = "voice_id"))]
    pub voice: String,
    pub name: String,
    #[serde(default)]
    pub category: String,
}

/// Why one of the user's own voices can't be kept or forgotten.
#[derive(Debug, PartialEq)]
pub enum OwnVoiceError {
    Invalid(String),
    /// The provider didn't speak it; the message says what it answered.
    Provider(String),
    /// Characters made here speak with it.
    InUse(Vec<String>),
    NotFound,
    Io(String),
}

/// Why a job can't start.
#[derive(Debug, PartialEq)]
pub enum StartError {
    Busy,
    Invalid(String),
    /// No config directory, or it can't be written.
    Unavailable(String),
}

/// Why a made Character can't be deleted.
#[derive(Debug, PartialEq)]
pub enum DeleteError {
    /// No Character of that id was made here (or the id isn't one).
    NotFound,
    /// Its job is running.
    Busy,
    Io(String),
}

/// What stopped a line.
#[derive(Debug, PartialEq)]
struct Stop {
    reason: Option<&'static str>,
    message: String,
}

impl Stop {
    fn because(reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            reason: Some(reason),
            message: message.into(),
        }
    }
}

impl From<String> for Stop {
    fn from(message: String) -> Self {
        Self {
            reason: None,
            message,
        }
    }
}

pub struct Maker {
    /// `config_dir()/characters`; None without a config directory, and in tests unless one sets it.
    dir: Option<PathBuf>,
    keys: Arc<VoiceKeys>,
    own: Arc<OwnVoices>,
    /// The providers' base URLs; others only in tests.
    elevenlabs_url: String,
    status: Mutex<Option<MakingStatus>>,
    changed: watch::Sender<()>,
}

impl Maker {
    pub fn new(
        dir: Option<PathBuf>,
        keys: Arc<VoiceKeys>,
        own: Arc<OwnVoices>,
    ) -> Self {
        Self {
            dir,
            keys,
            own,
            elevenlabs_url: crate::synthesis::ELEVENLABS_URL.to_owned(),
            status: Mutex::new(None),
            changed: watch::channel(()).0,
        }
    }

    /// Changes with each step of a job.
    pub fn watch(&self) -> watch::Receiver<()> {
        self.changed.subscribe()
    }

    pub fn status(&self) -> Option<MakingStatus> {
        self.lock().clone()
    }

    /// The status, even after a job panicked holding it.
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<MakingStatus>> {
        self.status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn report(&self, change: impl FnOnce(&mut MakingStatus)) {
        if let Some(status) = self.lock().as_mut() {
            change(status);
        }
        self.changed.send_replace(());
    }

    /// Runs a job; one that panics ends as failed, `internal`, rather than making forever.
    fn guard(&self, job: impl FnOnce()) {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
            self.report(|status| {
                status.state = "failed";
                status.reason = Some("internal");
                status.error = Some("making the Character stopped unexpectedly".to_owned());
            });
        }
    }

    /// The voices a made Character can speak with: ours, then the user's own.
    pub fn voices(&self) -> Vec<Voice> {
        voices()
            .into_iter()
            .chain(self.own.list().iter().map(Voice::from))
            .collect()
    }

    /// Tries a voice in the user's own account with their own key, saying `text`; keeps it when the provider speaks
    /// it, and answers it with what was said, as 16 kHz PCM.
    pub fn add_own(
        &self,
        provider: &str,
        voice: &str,
        name: &str,
        text: &str,
    ) -> Result<(Voice, Vec<i16>), OwnVoiceError> {
        let invalid = |message: String| OwnVoiceError::Invalid(message);
        let named = Provider::named(provider)
            .ok_or_else(|| invalid(format!("{provider} is not a provider: elevenlabs")))?;
        let (voice, name, text) = (voice.trim(), name.trim(), without_address(text.trim()));
        if voice.is_empty()
            || voice.len() > 128
            || voice.contains(char::is_whitespace)
        {
            return Err(invalid(
                "a voice id is 1 to 128 characters, with no spaces".to_owned(),
            ));
        }
        if name.is_empty() || name.chars().count() > 64 {
            return Err(invalid("a name must be 1 to 64 characters".to_owned()));
        }
        if text.is_empty() || text.encode_utf16().count() > MAX_TEXT {
            return Err(invalid(format!(
                "the line to try must be 1 to {MAX_TEXT} characters"
            )));
        }
        let key = self
            .keys
            .key(named)
            .ok_or_else(|| invalid(format!("no own key for {provider}")))?;
        match self.call(voice, &key, &text) {
            Ok(bytes) => {
                self.keys.refused(named, false);
                let samples = trim_and_normalize(&pcm(&bytes)).ok_or_else(|| {
                    OwnVoiceError::Provider("the line came back silent".to_owned())
                })?;
                let kept = self
                    .own
                    .add(provider, voice, name)
                    .map_err(|error| OwnVoiceError::Io(error.to_string()))?;
                info!(provider, "kept a voice of the user's own");
                Ok((Voice::from(&kept), samples))
            }
            Err((status, message)) => {
                if status == Some(401) {
                    self.keys.refused(named, true);
                }
                Err(OwnVoiceError::Provider(message))
            }
        }
    }

    /// The voices in the account of the user's own ElevenLabs key, to pick one from rather than paste its id: the
    /// key may belong to an account the user never signs in to. A key without the voices permission is answered
    /// with what ElevenLabs said, and is not marked refused, since it still speaks.
    pub fn elevenlabs_voices(&self) -> Result<Vec<ProviderVoice>, OwnVoiceError> {
        #[derive(Deserialize)]
        struct Library {
            voices: Vec<ProviderVoice>,
        }
        let key = self
            .keys
            .key(Provider::ElevenLabs)
            .ok_or_else(|| OwnVoiceError::Invalid("no own key for elevenlabs".to_owned()))?;
        let url = format!("{}/v1/voices", self.elevenlabs_url);
        let mut response = agent()
            .get(&url)
            .header("xi-api-key", &key)
            .call()
            .map_err(|error| OwnVoiceError::Provider(format!("calling ElevenLabs: {error}")))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_AUDIO)
            .read_to_string()
            .unwrap_or_default();
        if status != 200 {
            return Err(OwnVoiceError::Provider(format!(
                "ElevenLabs answered {status}: {}",
                body.trim().chars().take(200).collect::<String>()
            )));
        }
        let library: Library = serde_json::from_str(&body).map_err(|error| {
            OwnVoiceError::Provider(format!("reading ElevenLabs' voices: {error}"))
        })?;
        let mut voices = library.voices;
        // The account's own voices first, then ElevenLabs' stock ones, each by name.
        voices.sort_by_key(|voice| (voice.category == "premade", voice.name.to_lowercase()));
        Ok(voices)
    }

    /// Forgets one of the user's own voices; not while a Character made here speaks with it.
    pub fn remove_own(&self, id: &str) -> Result<(), OwnVoiceError> {
        let using: Vec<String> = self
            .list()
            .into_iter()
            .filter(|made| made.voice == id)
            .map(|made| made.name)
            .collect();
        if !using.is_empty() {
            return Err(OwnVoiceError::InUse(using));
        }
        match self.own.remove(id) {
            Ok(true) => Ok(()),
            Ok(false) => Err(OwnVoiceError::NotFound),
            Err(error) => Err(OwnVoiceError::Io(error.to_string())),
        }
    }

    /// The Characters made here, by id: what was asked, and whether its packs are made.
    pub fn list(&self) -> Vec<MadeCharacter> {
        let Some(entries) = self
            .dir
            .as_ref()
            .and_then(|dir| std::fs::read_dir(dir).ok())
        else {
            return Vec::new();
        };
        let mut made: Vec<MadeCharacter> = entries
            .flatten()
            .filter_map(|entry| {
                let id = entry
                    .file_name()
                    .into_string()
                    .ok()
                    .filter(|id| valid_id(id))?;
                let manifest: Request = serde_json::from_str(
                    &std::fs::read_to_string(entry.path().join(MANIFEST)).ok()?,
                )
                .ok()?;
                let complete = entry.path().join("pack.bin").is_file();
                Some(MadeCharacter {
                    id,
                    name: manifest.name,
                    base: manifest.base,
                    voice: manifest.voice,
                    lines: manifest.lines.len(),
                    complete,
                })
            })
            .collect();
        made.sort_by(|a, b| a.id.cmp(&b.id));
        made
    }

    /// Deletes a Character made here, its packs, line audio, manifest and the App's look with it; not while its job runs.
    pub fn delete(&self, id: &str) -> Result<(), DeleteError> {
        let dir = self
            .dir
            .as_ref()
            .filter(|_| valid_id(id))
            .map(|dir| dir.join(id))
            .ok_or(DeleteError::NotFound)?;
        // Held while removing, so the job can't start again halfway.
        let status = self.lock();
        if status
            .as_ref()
            .is_some_and(|status| status.state == "making" && status.id == id)
        {
            return Err(DeleteError::Busy);
        }
        if !dir.join(MANIFEST).is_file() {
            return Err(DeleteError::NotFound);
        }
        std::fs::remove_dir_all(&dir)
            .map_err(|error| DeleteError::Io(format!("deleting {}: {error}", dir.display())))?;
        info!(id, "deleted a made Character");
        Ok(())
    }

    /// Checks the request and starts making it on a thread of its own; answers the job's first status.
    pub fn start(self: &Arc<Self>, request: Request) -> Result<MakingStatus, StartError> {
        let (id, voice) = self.begin(&request)?;
        let maker = self.clone();
        std::thread::spawn(move || maker.guard(|| maker.make(&id, &voice, &request)));
        Ok(self.status().expect("begun"))
    }

    /// Checks the request, settles the Character's id and marks the job as making.
    pub(crate) fn begin(&self, request: &Request) -> Result<(String, Voice), StartError> {
        let dir = self
            .dir
            .as_ref()
            .ok_or_else(|| StartError::Unavailable("no config directory".to_owned()))?;
        let voice = check(request, &self.voices()).map_err(StartError::Invalid)?;
        let id = match &request.id {
            Some(id) if valid_id(id) && dir.join(id).join(MANIFEST).is_file() => id.clone(),
            Some(id) => {
                return Err(StartError::Invalid(format!(
                    "{id} is not a Character made here"
                )));
            }
            None => new_id(&request.name).map_err(StartError::Unavailable)?,
        };
        let mut status = self.lock();
        if status
            .as_ref()
            .is_some_and(|status| status.state == "making")
        {
            return Err(StartError::Busy);
        }
        *status = Some(MakingStatus {
            id: id.clone(),
            name: request.name.trim().to_owned(),
            state: "making",
            done: 0,
            total: 0,
            reason: None,
            error: None,
        });
        drop(status);
        self.changed.send_replace(());
        Ok((id, voice))
    }

    /// Makes every line not made yet, then the packs; reports each step.
    fn make(&self, id: &str, voice: &Voice, request: &Request) {
        let dir = self.dir.as_ref().expect("begun").join(id);
        match self.make_in(&dir, id, voice, request) {
            Ok(()) => {
                info!(id, "made a Character");
                self.report(|status| status.state = "done");
            }
            Err(stop) => {
                warn!(id, reason = stop.reason, error = %stop.message, "making a Character stopped");
                self.report(|status| {
                    status.state = "failed";
                    status.reason = stop.reason;
                    status.error = Some(stop.message);
                });
            }
        }
    }

    fn make_in(&self, dir: &Path, id: &str, voice: &Voice, request: &Request) -> Result<(), Stop> {
        let io = |error: std::io::Error| Stop::from(format!("writing {}: {error}", dir.display()));
        // Lines made in another voice are no use to this one.
        let before: Option<Request> = std::fs::read_to_string(dir.join(MANIFEST))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok());
        if before.is_some_and(|before| before.voice != request.voice) {
            for gone in ["audio", "audio-address"] {
                let _ = std::fs::remove_dir_all(dir.join(gone));
            }
        }
        std::fs::create_dir_all(dir).map_err(io)?;
        write(
            &dir.join(MANIFEST),
            serde_json::to_string_pretty(request)
                .expect("serializes")
                .as_bytes(),
        )
        .map_err(io)?;
        // The packs are of the lines before; they come back once these are made.
        let _ = std::fs::remove_file(dir.join("pack.bin"));
        let _ = std::fs::remove_dir_all(dir.join("address"));

        let language = if voice.own {
            preset_language(&request.base)
        } else {
            voice.language.clone()
        };
        let forms = forms(&language);
        let pieces = pieces(&request.lines, &forms);
        let made = pieces
            .iter()
            .filter(|piece| dir.join(&piece.path).is_file())
            .count();
        self.report(|status| (status.done, status.total) = (made, pieces.len()));
        for (done, piece) in (made + 1..).zip(
            pieces
                .iter()
                .filter(|piece| !dir.join(&piece.path).is_file()),
        ) {
            let samples = trim_and_normalize(&self.synthesize(voice, &piece.text)?)
                .ok_or_else(|| format!("the line came back silent: {}", piece.text))?;
            let path = dir.join(&piece.path);
            std::fs::create_dir_all(path.parent().expect("in a pool")).map_err(io)?;
            write(
                &path.with_extension("txt"),
                format!("{}\n", piece.text).as_bytes(),
            )
            .map_err(io)?;
            write(
                &path,
                &samples
                    .iter()
                    .flat_map(|sample| sample.to_le_bytes())
                    .collect::<Vec<_>>(),
            )
            .map_err(io)?;
            self.report(|status| status.done = done);
        }

        // Drop the audio of lines that were deleted or reworded, as make-character.sh does.
        let keep: std::collections::HashSet<PathBuf> =
            pieces.iter().map(|piece| dir.join(&piece.path)).collect();
        for root in ["audio", "audio-address"] {
            prune(&dir.join(root), &keep);
        }
        write(
            &dir.join("pack.bin"),
            &pack(id, &pools(&dir.join("audio")))?,
        )
        .map_err(io)?;
        for (form, _) in &forms {
            let audio = dir.join("audio-address").join(form);
            if audio.is_dir() {
                std::fs::create_dir_all(dir.join("address")).map_err(io)?;
                write(
                    &dir.join("address").join(format!("{form}.bin")),
                    &pack(id, &pools(&audio))?,
                )
                .map_err(io)?;
            }
        }
        Ok(())
    }

    fn synthesize(&self, voice: &Voice, text: &str) -> Result<Vec<i16>, Stop> {
        let provider = Provider::named(&voice.provider)
            .ok_or_else(|| format!("{} is not a provider", voice.provider))?;
        let Some(key) = self.keys.key(provider) else {
            return Err(Stop::because(
                "own_key",
                format!("set your own {} key to make {}", voice.provider, voice.name),
            ));
        };
        Ok(pcm(&self.own(provider, &voice.voice, &key, text)?))
    }

    /// A line from the provider with the user's own key; a 401 or 403 marks the key refused.
    fn own(
        &self,
        provider: Provider,
        voice: &str,
        key: &str,
        text: &str,
    ) -> Result<Vec<u8>, Stop> {
        match self.call(voice, key, text) {
            Ok(audio) => {
                self.keys.refused(provider, false);
                Ok(audio)
            }
            Err((Some(status @ (401 | 403)), _)) => {
                self.keys.refused(provider, true);
                Err(Stop::because(
                    "own_key",
                    format!("ElevenLabs refused your own key ({status})"),
                ))
            }
            Err((_, message)) => Err(message.into()),
        }
    }

    fn call(
        &self,
        voice: &str,
        key: &str,
        text: &str,
    ) -> Result<Vec<u8>, (Option<u16>, String)> {
        let name = "ElevenLabs";
        let url = format!(
            "{}/v1/text-to-speech/{voice}/stream?output_format=pcm_16000",
            self.elevenlabs_url
        );
        let mut response = agent()
            .post(&url)
            .header("xi-api-key", key)
            .header("Accept", "audio/pcm")
            .header("Content-Type", "application/json")
            .send(
                json!({"text": text, "model_id": ELEVENLABS_MODEL})
                    .to_string()
                    .as_bytes(),
            )
            .map_err(|error| (None, format!("calling {name}: {error}")))?;
        let status = response.status().as_u16();
        if status != 200 {
            let answer = response
                .body_mut()
                .with_config()
                .limit(1024)
                .read_to_string()
                .unwrap_or_default();
            return Err((
                Some(status),
                format!(
                    "{name} answered {status}: {}",
                    answer.trim().chars().take(200).collect::<String>()
                ),
            ));
        }
        read_all(&mut response, name).map_err(|stop| (None, stop.message))
    }
}

fn agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| crate::speech::agent(crate::synthesis::TIMEOUT))
}

fn read_all(response: &mut ureq::http::Response<ureq::Body>, from: &str) -> Result<Vec<u8>, Stop> {
    Ok(response
        .body_mut()
        .with_config()
        .limit(MAX_AUDIO)
        .read_to_vec()
        .map_err(|error| format!("reading audio from {from}: {error}"))?)
}

/// The voice, of `voices`, if the request can be made.
fn check(request: &Request, voices: &[Voice]) -> Result<Voice, String> {
    let name = request.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err("a name must be 1 to 64 characters".to_owned());
    }
    if !crate::synthesis::VOICE_FILES
        .iter()
        .any(|(id, _)| *id == request.base)
    {
        return Err(format!("{} is not a preset Character", request.base));
    }
    let voice = voices
        .iter()
        .find(|voice| voice.id == request.voice)
        .cloned()
        .ok_or_else(|| format!("{} is not in voices.tsv or your own voices", request.voice))?;
    if request.lines.is_empty() || request.lines.len() > MAX_LINES {
        return Err(format!("a Character has 1 to {MAX_LINES} lines"));
    }
    for line in &request.lines {
        if Occasion::from_name(&line.occasion).is_none() {
            return Err(format!("{} is not an occasion", line.occasion));
        }
        let units = line.text.encode_utf16().count();
        if line.text.trim().is_empty() || units > MAX_TEXT || line.text.contains(['\n', '\r', '\t'])
        {
            return Err(format!(
                "a line must be one row of 1 to {MAX_TEXT} characters: {:?}",
                line.text
            ));
        }
    }
    Ok(voice)
}

/// A made Character's id: lowercase letters, digits and dashes, at most 31, as a pack's id may be.
fn valid_id(id: &str) -> bool {
    (1..=31).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// The slug of the name and eight random hex digits.
fn new_id(name: &str) -> Result<String, String> {
    let mut slug = String::new();
    for character in name.to_lowercase().chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_end_matches('-').chars().take(22).collect();
    let slug = if slug.is_empty() {
        "character"
    } else {
        slug.trim_end_matches('-')
    };
    let mut bytes = [0u8; 4];
    getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
    Ok(format!("{slug}-{:08x}", u32::from_le_bytes(bytes)))
}

/// Written whole or not at all, so a line cut short by a crash is made again.
fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension("part");
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(temporary, path)
}

fn prune(root: &Path, keep: &std::collections::HashSet<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            prune(&path, keep);
            let _ = std::fs::remove_dir(&path);
        } else if path.extension().is_some_and(|extension| extension == "pcm")
            && !keep.contains(&path)
        {
            let _ = std::fs::remove_file(path.with_extension("txt"));
            let _ = std::fs::remove_file(&path);
        }
    }
    let _ = std::fs::remove_dir(root);
}

/// Each occasion's lines in a directory as `tools/character_pack.py` reads them: `<occasion>/*.pcm` in name order.
fn pools(audio: &Path) -> Vec<Vec<Vec<i16>>> {
    Occasion::ALL
        .iter()
        .map(|occasion| {
            let mut files: Vec<PathBuf> = std::fs::read_dir(audio.join(occasion.name()))
                .map(|entries| {
                    entries
                        .flatten()
                        .map(|entry| entry.path())
                        .filter(|path| path.extension().is_some_and(|extension| extension == "pcm"))
                        .collect()
                })
                .unwrap_or_default();
            files.sort();
            files
                .iter()
                .filter_map(|path| std::fs::read(path).ok())
                .map(|bytes| pcm(&bytes))
                .collect()
        })
        .collect()
}

/// 16 kHz mono samples as a WAV file, for the App to play a voice tried.
pub fn wav(samples: &[i16]) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    out.extend(samples.iter().flat_map(|sample| sample.to_le_bytes()));
    out
}

/// 16-bit little-endian PCM as samples.
fn pcm(bytes: &[u8]) -> Vec<i16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| i16::from_le_bytes(*pair))
        .collect()
}

/// TTS pads both ends with silence; trimmed at -50 dBFS, the box speaks as soon as it is asked to. Then the peak goes
/// to -1 dBFS. None when nothing is left.
fn trim_and_normalize(samples: &[i16]) -> Option<Vec<i16>> {
    let threshold = 32768.0 * 10f64.powf(-50.0 / 20.0);
    let loud = |sample: &i16| f64::from(sample.unsigned_abs()) > threshold;
    let start = samples.iter().position(loud)?;
    let end = samples.iter().rposition(loud)? + 1;
    let trimmed = &samples[start..end];
    let peak = f64::from(trimmed.iter().map(|sample| sample.unsigned_abs()).max()?);
    let gain = 32768.0 * 10f64.powf(-1.0 / 20.0) / peak;
    Some(
        trimmed
            .iter()
            .map(|&sample| (f64::from(sample) * gain).round().clamp(-32768.0, 32767.0) as i16)
            .collect(),
    )
}

/// IMA ADPCM with the box's own encoder, two samples a byte, low nibble first.
fn encode(samples: &[i16]) -> Vec<u8> {
    let mut encoder = Encoder::default();
    samples
        .chunks(2)
        .map(|pair| {
            if let [first, second] = pair {
                encoder.encode_pair(*first, *second)
            } else {
                encoder.encode(pair[0])
            }
        })
        .collect()
}

/// A Character pack with no look, byte for byte as `tools/character_pack.py` builds it.
fn pack(id: &str, pools: &[Vec<Vec<i16>>]) -> Result<Vec<u8>, String> {
    use vibebuddy_firmware_core::character_pack::{HEADER_BYTES, MAGIC, MAX_LINES};
    let mut header = vec![0u8; HEADER_BYTES];
    let mut payload = Vec::new();
    let mut line = 0usize;
    let put = |header: &mut Vec<u8>, at: usize, bytes: &[u8]| {
        header[at..at + bytes.len()].copy_from_slice(bytes)
    };
    for (slot, lines) in pools.iter().enumerate() {
        put(
            &mut header,
            56 + slot * 4,
            &(if lines.is_empty() { 0 } else { line as u16 }).to_le_bytes(),
        );
        put(
            &mut header,
            58 + slot * 4,
            &(lines.len() as u16).to_le_bytes(),
        );
        for samples in lines {
            if line == MAX_LINES {
                return Err(format!("more than {MAX_LINES} lines"));
            }
            put(
                &mut header,
                128 + line * 8,
                &((HEADER_BYTES + payload.len()) as u32).to_le_bytes(),
            );
            put(
                &mut header,
                132 + line * 8,
                &(samples.len() as u32).to_le_bytes(),
            );
            payload.extend(encode(samples));
            line += 1;
        }
    }
    put(&mut header, 0, MAGIC);
    put(&mut header, 4, &1u32.to_le_bytes());
    put(&mut header, 8, &(payload.len() as u32).to_le_bytes());
    put(&mut header, 12, &crc32fast::hash(&payload).to_le_bytes());
    put(&mut header, 16, id.as_bytes());
    put(&mut header, 48, &SAMPLE_RATE.to_le_bytes());
    put(&mut header, 52, &[1, pools.len() as u8]);
    put(&mut header, 54, &(line as u16).to_le_bytes());
    let crc = crc32fast::hash(&header[..1020]);
    put(&mut header, 1020, &crc.to_le_bytes());
    header.extend(payload);
    Ok(header)
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use vibebuddy_firmware_core::adpcm::Decoder;
    use vibebuddy_firmware_core::character_pack;

    use super::*;

    fn line(occasion: &str, text: &str) -> Line {
        Line {
            occasion: occasion.to_owned(),
            text: text.to_owned(),
        }
    }

    fn request(voice: &str, lines: Vec<Line>) -> Request {
        Request {
            id: None,
            name: "Night Owl".to_owned(),
            base: "ada".to_owned(),
            voice: voice.to_owned(),
            lines,
        }
    }

    /// A line as a TTS answers it: silence, a tone, silence.
    fn spoken() -> Vec<u8> {
        let tone = (0..1600).map(|n| (8000.0 * (n as f64 * 0.2).sin()) as i16);
        std::iter::repeat_n(0i16, 400)
            .chain(tone)
            .chain(std::iter::repeat_n(3, 400))
            .flat_map(i16::to_le_bytes)
            .collect()
    }

    fn answer(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        [head.as_bytes(), body].concat()
    }

    /// A fake HTTP server answering the nth request with `respond(n, request)`; returns its base URL and the requests.
    fn serve(
        respond: impl Fn(usize, &str) -> Vec<u8> + Send + 'static,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        std::thread::spawn(move || {
            for socket in listener.incoming() {
                let mut socket = socket.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let text = String::from_utf8_lossy(&request).into_owned();
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let length = head
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")?
                                    .parse::<usize>()
                                    .ok()
                            })
                            .unwrap_or(0);
                        if body.len() >= length {
                            break;
                        }
                    }
                    let read = socket.read(&mut buffer).unwrap();
                    request.extend_from_slice(&buffer[..read]);
                }
                let request = String::from_utf8_lossy(&request).into_owned();
                let number = seen.lock().unwrap().len();
                seen.lock().unwrap().push(request.clone());
                socket.write_all(&respond(number, &request)).unwrap();
            }
        });
        (base, requests)
    }

    fn maker(tag: &str, keys: &[(Provider, &str)], elevenlabs: &str) -> (Maker, PathBuf) {
        let scratch = crate::voice_keys::tests::scratch(tag);
        let own = Arc::new(VoiceKeys::new(Some(&scratch)));
        own.set(
            &keys
                .iter()
                .map(|(provider, key)| (*provider, Some((*key).to_owned())))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let mut maker = Maker::new(
            Some(scratch.join("characters")),
            own,
            Arc::new(OwnVoices::new(Some(&scratch))),
        );
        maker.elevenlabs_url = elevenlabs.to_owned();
        (maker, scratch)
    }

    /// Makes `request` on this thread; answers the status it ended with.
    fn run(maker: &Maker, request: &Request) -> MakingStatus {
        let (id, voice) = maker.begin(request).unwrap();
        maker.make(&id, &voice, request);
        maker.status().unwrap()
    }

    fn text_of(request: &str) -> String {
        let body: serde_json::Value =
            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        body["text"].as_str().unwrap().to_owned()
    }

    #[test]
    fn a_form_of_address_goes_with_its_comma_as_in_make_character() {
        assert_eq!(without_address("All done, {address}."), "All done.");
        assert_eq!(
            without_address("Hey {address}, need you."),
            "Hey, need you."
        );
        assert_eq!(without_address("{address}，快来看看。"), "快来看看。");
        assert_eq!(without_address("好了，{address}！"), "好了！");
        assert_eq!(
            without_address("都这么晚了，{address}快去睡"),
            "都这么晚了，快去睡"
        );
        assert_eq!(without_address("Nice one {address}"), "Nice one");
    }

    #[test]
    fn lines_with_an_address_are_made_once_per_form_and_named_as_make_character_names_them() {
        let lines = [
            line("done", "Nice."),
            line("done", "Done, {address}."),
            line("failed", "Oops."),
        ];
        let pieces = pieces(&lines, &forms("en"));
        let paths: Vec<String> = pieces
            .iter()
            .map(|piece| piece.path.display().to_string())
            .collect();
        // `printf '%s' 'Done, boss.' | shasum -a 1` starts 1641ab442f.
        assert!(
            paths.contains(&"audio-address/boss/done/0-002-1641ab442f.pcm".to_owned()),
            "{paths:?}"
        );
        let texts: Vec<&str> = pieces.iter().map(|piece| piece.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "Nice.",
                "Done.",
                "Done, boss.",
                "Done, captain.",
                "Done, buddy.",
                "Oops."
            ]
        );
        assert!(
            paths[0].starts_with("audio/done/1-")
                && paths[1].starts_with("audio/done/0-002-")
                && paths[3].starts_with("audio-address/captain/done/0-002-")
        );
        assert_eq!(
            forms("zh")
                .iter()
                .map(|(form, _)| *form)
                .collect::<Vec<_>>(),
            ["laoban", "dalao", "ge", "jie", "qin"]
        );
    }

    #[test]
    fn adpcm_decodes_back_close_to_the_input() {
        let input: Vec<i16> = (0..4001)
            .map(|n| (20000.0 * (n as f64 * 0.05).sin() * (n as f64 * 0.001).cos()) as i16)
            .collect();
        let bytes = encode(&input);
        assert_eq!(bytes.len(), input.len().div_ceil(2));
        let mut decoder = Decoder::default();
        let output: Vec<i16> = bytes
            .iter()
            .flat_map(|byte| [byte & 15, byte >> 4])
            .take(input.len())
            .map(|nibble| decoder.decode(nibble))
            .collect();
        let error: f64 = input
            .iter()
            .zip(&output)
            .map(|(a, b)| (f64::from(*a) - f64::from(*b)).powi(2))
            .sum();
        let signal: f64 = input.iter().map(|a| f64::from(*a).powi(2)).sum();
        assert!(
            10.0 * (signal / error).log10() > 20.0,
            "signal to noise {}",
            10.0 * (signal / error).log10()
        );
    }

    #[test]
    fn silence_is_trimmed_and_the_peak_is_minus_1_dbfs() {
        let samples = pcm(&spoken());
        let out = trim_and_normalize(&samples).unwrap();
        assert!(out.len() < 1600 && out.len() > 1590, "{}", out.len());
        assert_eq!(
            out.iter().map(|sample| sample.unsigned_abs()).max(),
            Some(29205)
        );
        assert_eq!(
            trim_and_normalize(&[0, 50, -100, 3]),
            None,
            "all below -50 dBFS"
        );
    }

    #[test]
    fn the_pools_pack_byte_for_byte_as_character_pack_py_packs_them() {
        // daemon/src/fixtures/character: three lines of PCM, and pack.bin as tools/character_pack.py built it from them
        // (`tools/character_pack.py made-a1b2c3d4 daemon/src/fixtures/character/audio pack.bin`).
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/fixtures/character");
        let pools = pools(&fixture.join("audio"));
        assert_eq!(
            pools.iter().map(Vec::len).collect::<Vec<_>>(),
            [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
        );
        assert_eq!(
            pack("made-a1b2c3d4", &pools).unwrap(),
            std::fs::read(fixture.join("pack.bin")).unwrap()
        );
    }

    #[test]
    fn only_voices_in_voices_tsv_are_accepted() {
        let all = voices();
        // Every preset's voice, and the built-in default's, as `voice_for` names them.
        for character in [
            None,
            Some("ada"),
            Some("dave"),
            Some("hank"),
            Some("kai"),
            Some("luna"),
            Some("mei"),
            Some("wanwanxiaohe"),
        ] {
            let voice = crate::synthesis::voice_for(character);
            assert!(
                all.iter()
                    .any(|known| format!("{}:{}", known.provider, known.voice) == voice),
                "{voice} is missing"
            );
        }
        assert!(
            all.iter()
                .all(|voice| Provider::named(&voice.provider).is_some()
                    && ["en", "zh"].contains(&voice.language.as_str()))
        );
        let ok = request("luna", vec![line("done", "Done.")]);
        assert_eq!(
            check(&ok, &voices()).map(|voice| voice.voice),
            Ok("ggx46gxUmdtruayPIL8q".to_owned())
        );
        for (bad, why) in [
            (
                request("elevenlabs:ggx46gxUmdtruayPIL8q", ok.lines.clone()),
                "not in voices.tsv",
            ),
            (
                Request {
                    base: "robot".to_owned(),
                    ..ok.clone()
                },
                "not a preset",
            ),
            (
                request("luna", vec![line("lunch", "Food.")]),
                "not an occasion",
            ),
            (request("luna", vec![line("done", "Two\nrows")]), "one row"),
            (request("luna", vec![]), "1 to 110 lines"),
        ] {
            assert!(check(&bad, &voices()).unwrap_err().contains(why), "{why}");
        }
    }

    #[test]
    fn every_preset_a_character_can_start_from_has_its_lines_and_they_can_be_made() {
        for (id, _) in crate::synthesis::VOICE_FILES {
            let lines = preset_lines(id).unwrap_or_else(|| panic!("{id} has no lines"));
            assert_eq!(lines.len(), 55, "{id}");
            assert!(
                lines.iter().any(|line| line.text.contains("{address}")),
                "{id}"
            );
            let voice = voices().into_iter().find(|voice| voice.id == *id).unwrap();
            assert_eq!(
                check(
                    &Request {
                        id: None,
                        name: "Copy".to_owned(),
                        base: (*id).to_owned(),
                        voice: voice.id,
                        lines
                    },
                    &voices()
                )
                .err(),
                None,
                "{id}"
            );
        }
        assert_eq!(
            preset_lines("ada").unwrap()[0],
            line("input_required", "Hey {address}, it's waiting on you.")
        );
        assert_eq!(preset_lines("robot"), None);
    }

    #[test]
    fn a_refusal_halfway_stops_the_job_and_starting_again_makes_each_line_once() {
        // The provider turns away the third request, and is back for the next run.
        let refuse = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let refusing = refuse.clone();
        let (eleven, requests) = serve(move |n, _| match n {
            2 if refusing.load(std::sync::atomic::Ordering::SeqCst) => answer(
                "429 Too Many Requests",
                "application/json",
                br#"{"reason":"monthly"}"#,
            ),
            _ => answer("200 OK", "audio/pcm;rate=16000", &spoken()),
        });
        let (maker, scratch) = maker(
            "making-resume",
            &[(Provider::ElevenLabs, "sk_own")],
            &eleven,
        );
        let lines = vec![
            line("done", "All done."),
            line("done", "Done, {address}."),
            line("welcome_back", "Back again."),
        ];
        let mut request = request("ada", lines);

        let stopped = run(&maker, &request);
        assert_eq!(
            (stopped.state, stopped.reason, stopped.done, stopped.total),
            ("failed", None, 2, 6)
        );
        let dir = scratch.join("characters").join(&stopped.id);
        assert!(
            !dir.join("pack.bin").exists(),
            "no pack until every line is made"
        );
        let listed = maker.list();
        assert_eq!(
            listed,
            [MadeCharacter {
                id: stopped.id.clone(),
                name: "Night Owl".to_owned(),
                base: "ada".to_owned(),
                voice: "ada".to_owned(),
                lines: 3,
                complete: false
            }]
        );
        let first = requests.lock().unwrap().clone();
        assert!(
            first[0].to_ascii_lowercase().contains("xi-api-key: sk_own"),
            "{}",
            first[0]
        );
        assert!(
            first[0].contains("/v1/text-to-speech/tcSsCJFYrsHpK5rCQZMf/"),
            "{}",
            first[0]
        );

        refuse.store(false, std::sync::atomic::Ordering::SeqCst);
        request.id = Some(stopped.id.clone());
        let done = run(&maker, &request);
        assert_eq!(
            (done.state, done.reason, done.done, done.total),
            ("done", None, 6, 6)
        );
        assert!(maker.list()[0].complete);
        let all = requests.lock().unwrap().clone();
        // Each line answered with audio once: the refused one was asked again, nothing else was.
        let made: Vec<String> = all
            .iter()
            .enumerate()
            .filter(|(n, _)| *n != 2)
            .map(|(_, request)| text_of(request))
            .collect();
        let mut unique = made.clone();
        unique.sort();
        unique.dedup();
        assert_eq!((made.len(), unique.len()), (6, 6), "{made:?}");
        assert_eq!(
            text_of(&all[2]),
            text_of(&all[3]),
            "the refused line is the first made on resuming"
        );

        // The packs read as the box reads them: the address line first in its pool, then the other.
        let pack = std::fs::read(dir.join("pack.bin")).unwrap();
        let parsed = character_pack::parse(&pack, 1 << 21).unwrap();
        assert_eq!(parsed.id(), stopped.id);
        assert_eq!(
            (
                parsed.pool(Occasion::Done).len(),
                parsed.pool(Occasion::WelcomeBack).len()
            ),
            (2, 1)
        );
        for form in ["boss", "captain", "buddy"] {
            let variant = character_pack::parse(
                &std::fs::read(dir.join(format!("address/{form}.bin"))).unwrap(),
                1 << 21,
            )
            .unwrap();
            assert_eq!(
                (
                    variant.pool(Occasion::Done).len(),
                    variant.pool(Occasion::WelcomeBack).len()
                ),
                (1, 0),
                "{form}"
            );
        }
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(MANIFEST)).unwrap()).unwrap();
        assert_eq!(
            (
                manifest["name"].as_str(),
                manifest["voice"].as_str(),
                manifest["lines"].as_array().map(Vec::len)
            ),
            (Some("Night Owl"), Some("ada"), Some(3))
        );

        // A line dropped: nothing to synthesize, its audio goes.
        request.lines.pop();
        assert_eq!(run(&maker, &request).state, "done");
        assert_eq!(requests.lock().unwrap().len(), all.len());
        assert!(!dir.join("audio/welcome_back").exists());
        let _ = std::fs::remove_dir_all(scratch);
    }

    #[test]
    fn an_own_key_goes_to_its_provider_for_16_khz_pcm() {
        let (eleven, eleven_requests) = serve(|_, _| answer("200 OK", "audio/pcm", &spoken()));
        let keys = [(Provider::ElevenLabs, "sk_own_eleven")];
        let (maker, scratch) = maker("making-own", &keys, &eleven);

        let made = run(&maker, &request("hank", vec![line("done", "Done.")]));
        assert_eq!(made.state, "done", "{made:?}");
        let sent = eleven_requests.lock().unwrap()[0].clone();
        assert!(sent.starts_with("POST /v1/text-to-speech/U6jH23hcPh0zH17nf3DT/stream?output_format=pcm_16000 HTTP/1.1\r\n"), "{sent}");
        assert!(
            sent.to_ascii_lowercase()
                .contains("xi-api-key: sk_own_eleven")
                && sent.contains(r#""model_id":"eleven_flash_v2_5""#),
            "{sent}"
        );
        let pack =
            std::fs::read(scratch.join("characters").join(&made.id).join("pack.bin")).unwrap();
        let line = character_pack::parse(&pack, 1 << 21)
            .unwrap()
            .pool(Occasion::Done)[0];
        assert!(
            line.samples > 1590 && line.samples < 1600,
            "the line, trimmed: {}",
            line.samples
        );
        let _ = std::fs::remove_dir_all(scratch);
    }

    #[test]
    fn without_an_own_key_a_character_cannot_be_made() {
        let (maker, scratch) = maker("making-no-key", &[], "http://127.0.0.1:9");
        let stopped = run(&maker, &request("kai", vec![line("done", "Done.")]));
        assert_eq!(
            (stopped.state, stopped.reason),
            ("failed", Some("own_key")),
            "{stopped:?}"
        );
        let _ = std::fs::remove_dir_all(scratch);
    }

    #[test]
    fn a_job_that_panics_ends_as_failed_not_making_forever() {
        let (maker, scratch) = maker(
            "making-panic",
            &[],
            "http://127.0.0.1:9",
        );
        let request = request("ada", vec![line("done", "Done.")]);
        maker.begin(&request).unwrap();
        maker.guard(|| {
            let _held = maker.status.lock().unwrap();
            panic!("a bug");
        });
        let status = maker.status().unwrap();
        assert_eq!((status.state, status.reason), ("failed", Some("internal")));
        assert!(maker.begin(&request).is_ok(), "the next one can start");
        let _ = std::fs::remove_dir_all(scratch);
    }

    #[test]
    fn a_made_character_is_deleted_but_not_while_it_is_being_made_nor_outside_its_directory() {
        let (maker, scratch) = maker(
            "making-delete",
            &[],
            "http://127.0.0.1:9",
        );
        let request = request("ada", vec![line("done", "Done.")]);
        let (id, _) = maker.begin(&request).unwrap();
        let dir = scratch.join("characters").join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MANIFEST), serde_json::to_string(&request).unwrap()).unwrap();
        std::fs::write(dir.join("look.bin"), b"look").unwrap();
        assert_eq!(maker.delete(&id), Err(DeleteError::Busy));
        assert!(dir.is_dir());
        maker.report(|status| status.state = "failed");
        // Something next to the Characters, which a crafted id would reach.
        std::fs::write(scratch.join(MANIFEST), "{}").unwrap();
        for bad in ["..", "../characters", "a/b", "", "Night-Owl"] {
            assert_eq!(maker.delete(bad), Err(DeleteError::NotFound), "{bad:?}");
        }
        assert!(scratch.join(MANIFEST).is_file() && scratch.join("characters").is_dir());
        assert_eq!(maker.delete(&id), Ok(()));
        assert!(!dir.exists() && maker.list().is_empty());
        assert_eq!(maker.delete(&id), Err(DeleteError::NotFound));
        let _ = std::fs::remove_dir_all(scratch);
    }

    #[test]
    fn one_character_at_a_time_and_only_one_made_here_is_made_again() {
        let (maker, scratch) = maker(
            "making-busy",
            &[],
            "http://127.0.0.1:9",
        );
        let mut request = request("ada", vec![line("done", "Done.")]);
        let (id, _) = maker.begin(&request).unwrap();
        assert!(valid_id(&id) && id.starts_with("night-owl-"), "{id}");
        assert_eq!(maker.begin(&request).unwrap_err(), StartError::Busy);
        request.id = Some("../../elsewhere".to_owned());
        assert!(matches!(maker.begin(&request), Err(StartError::Invalid(_))));
        assert!(new_id("小何").unwrap().starts_with("character-"));
        assert_eq!(
            new_id("A very long name for a Character indeed")
                .unwrap()
                .len(),
            31
        );
        let _ = std::fs::remove_dir_all(scratch);
    }

    #[test]
    fn a_voice_of_the_users_own_is_tried_and_kept_and_makes_lines_in_the_presets_language() {
        let (eleven, requests) = serve(|_, _| answer("200 OK", "audio/pcm", &spoken()));
        let (maker, scratch) = maker(
            "own-eleven",
            &[(Provider::ElevenLabs, "sk_own")],
            &eleven,
        );

        let (voice, samples) = maker
            .add_own("elevenlabs", " me1 ", "My voice", "All done, {address}.")
            .unwrap();
        assert!(voice.own && voice.id.starts_with("own-") && voice.language.is_empty());
        assert_eq!(voice.voice, "me1");
        assert!(
            samples.len() > 1590 && samples.len() < 1600,
            "{}",
            samples.len()
        );
        assert_eq!(text_of(&requests.lock().unwrap()[0]), "All done.");
        assert!(
            maker.voices().iter().any(|listed| listed.id == voice.id),
            "listed after ours"
        );

        let made = run(
            &maker,
            &request(&voice.id, vec![line("done", "Done, {address}.")]),
        );
        assert_eq!(made.state, "done", "{made:?}");
        let dir = scratch.join("characters").join(&made.id);
        assert!(
            dir.join("address").join("boss.bin").is_file(),
            "Ada's English forms of address"
        );
        assert!(!dir.join("address").join("ge.bin").exists());
        let _ = std::fs::remove_dir_all(scratch);
    }

    #[test]
    fn a_voice_of_the_users_own_needs_their_key() {
        let (eleven, requests) = serve(|_, _| {
            answer(
                "401 Unauthorized",
                "application/json",
                br#"{"detail":"invalid key"}"#,
            )
        });
        let (maker, scratch) = maker(
            "own-no-key",
            &[],
            &eleven,
        );
        assert!(
            matches!(maker.add_own("elevenlabs", "abc", "Me", "Hi."), Err(OwnVoiceError::Invalid(why)) if why.contains("no own key"))
        );
        for bad in [
            ("eleven", "abc", "Me"),
            ("elevenlabs", "a b", "Me"),
            ("elevenlabs", "abc", " "),
        ] {
            assert!(
                matches!(
                    maker.add_own(bad.0, bad.1, bad.2, "Hi."),
                    Err(OwnVoiceError::Invalid(_))
                ),
                "{bad:?}"
            );
        }

        maker
            .keys
            .set(&[(Provider::ElevenLabs, Some("sk_wrong".to_owned()))])
            .unwrap();
        assert!(
            matches!(maker.add_own("elevenlabs", "abc", "Me", "Hi."), Err(OwnVoiceError::Provider(why)) if why.contains("401"))
        );
        assert!(maker.keys.status().elevenlabs.refused);
        assert!(
            maker.own.list().is_empty(),
            "nothing kept that didn't speak"
        );
        assert_eq!(requests.lock().unwrap().len(), 1);

        // A Character in a voice of the user's own, after the key is gone: it stops.
        let own = maker.own.add("elevenlabs", "abc", "Me").unwrap();
        maker.keys.set(&[(Provider::ElevenLabs, None)]).unwrap();
        let stopped = run(&maker, &request(&own.id, vec![line("done", "Done.")]));
        assert_eq!(
            (stopped.state, stopped.reason),
            ("failed", Some("own_key")),
            "{stopped:?}"
        );
        let _ = std::fs::remove_dir_all(scratch);
    }

    #[test]
    fn a_character_in_a_voice_of_the_users_own_replies_in_it_while_their_key_works_else_in_its_presets()
     {
        let (maker, scratch) = maker(
            "own-reply",
            &[(Provider::ElevenLabs, "sk_own")],
            "http://127.0.0.1:9",
        );
        let own = maker.own.add("elevenlabs", "me1", "Me").unwrap();
        let dir = scratch.join("characters");
        std::fs::create_dir_all(dir.join("mine-00000001")).unwrap();
        let manifest = Request {
            id: None,
            name: "Mine".to_owned(),
            base: "ada".to_owned(),
            voice: own.id.clone(),
            lines: vec![line("done", "Done.")],
        };
        std::fs::write(
            dir.join("mine-00000001").join(MANIFEST),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
        let reply =
            |maker: &Maker| voice_with(&dir, "mine-00000001", Some((&maker.own, &maker.keys)));

        assert_eq!(reply(&maker).as_deref(), Some("elevenlabs:me1"));
        maker.keys.refused(Provider::ElevenLabs, true);
        assert_eq!(
            reply(&maker),
            Some(crate::synthesis::voice_for(Some("ada")))
        );
        maker.keys.set(&[(Provider::ElevenLabs, None)]).unwrap();
        assert_eq!(
            reply(&maker),
            Some(crate::synthesis::voice_for(Some("ada")))
        );

        // Not forgotten while a Character speaks with it.
        assert_eq!(
            maker.remove_own(&own.id),
            Err(OwnVoiceError::InUse(vec!["Mine".to_owned()]))
        );
        std::fs::remove_dir_all(dir.join("mine-00000001")).unwrap();
        assert_eq!(maker.remove_own(&own.id), Ok(()));
        assert_eq!(maker.remove_own(&own.id), Err(OwnVoiceError::NotFound));
        let _ = std::fs::remove_dir_all(scratch);
    }

    #[test]
    fn a_tried_line_is_a_16_khz_wav() {
        let wav = wav(&[1, -1]);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[24..28], &16_000u32.to_le_bytes());
        assert_eq!(wav.len(), 48);
    }

    #[test]
    fn the_own_elevenlabs_keys_voices_are_listed_its_own_first_and_a_missing_permission_is_said() {
        let library = br#"{"voices":[{"voice_id":"p1","name":"Rachel","category":"premade","labels":{}},{"voice_id":"c1","name":"me","category":"cloned"}]}"#;
        let (eleven, requests) = serve(move |number, _| {
            if number == 0 {
                answer("200 OK", "application/json", library)
            } else {
                answer(
                    "401 Unauthorized",
                    "application/json",
                    br#"{"detail":{"status":"missing_permissions","message":"voices_read"}}"#,
                )
            }
        });
        let (maker, scratch) = maker(
            "own-eleven-list",
            &[(Provider::ElevenLabs, "sk_own")],
            &eleven,
        );
        let voices = maker.elevenlabs_voices().unwrap();
        assert_eq!(
            voices
                .iter()
                .map(|voice| voice.voice.as_str())
                .collect::<Vec<_>>(),
            ["c1", "p1"]
        );
        assert_eq!(
            (voices[0].name.as_str(), voices[0].category.as_str()),
            ("me", "cloned")
        );
        let sent = requests.lock().unwrap()[0].clone();
        assert!(
            sent.starts_with("GET /v1/voices HTTP/1.1\r\n")
                && sent.to_ascii_lowercase().contains("xi-api-key: sk_own"),
            "{sent}"
        );
        assert!(
            matches!(maker.elevenlabs_voices(), Err(OwnVoiceError::Provider(why)) if why.contains("missing_permissions"))
        );
        assert!(
            !maker.keys.status().elevenlabs.refused,
            "a key without the voices permission still speaks"
        );
        maker.keys.set(&[(Provider::ElevenLabs, None)]).unwrap();
        assert!(matches!(
            maker.elevenlabs_voices(),
            Err(OwnVoiceError::Invalid(_))
        ));
        let _ = std::fs::remove_dir_all(scratch);
    }
}
