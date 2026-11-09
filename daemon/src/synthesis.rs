//! Speech synthesis for spoken replies (ADR-0017): a reply's text in, an MP3 stream out, piece by piece as the
//! provider makes it.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use crate::voice_keys::{Provider, VoiceKeys};

/// The most text ElevenLabs is sent in one request, in UTF-16 code units.
pub const MAX_TEXT: usize = 1000;
/// A request gets this long for its whole answer; 1000 characters is about a minute of speech, made faster than
/// it plays.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(120);
/// No reply needs more: ten minutes at 48 kbps.
const MAX_AUDIO: usize = 4 * 1024 * 1024;

/// Turns one reply into speech. Blocking: it is called on a blocking thread.
pub trait Synthesizer: Send + Sync {
    /// Calls `on_audio` with each piece of the MP3 as it arrives (not on frame boundaries); `on_audio` returns false
    /// to stop. An error after some audio means the stream was cut short.
    fn synthesize(
        &self,
        text: &str,
        voice: &str,
        on_audio: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<(), String>;

    /// Whether it can speak in `voice` at all; when not, the reply stays text and nothing is tried.
    fn speaks(&self, _voice: &str) -> bool {
        true
    }
}

/// Hands on an MP3 body as it arrives; false when `on_audio` stopped it.
fn forward(
    mut reader: impl Read,
    host: &str,
    on_audio: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<bool, String> {
    let mut buffer = [0u8; 4096];
    let mut total = 0;
    loop {
        // A connection cut before the end of the chunked body is an error here, not an end.
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("reading audio from {host}: {error}"))?;
        if read == 0 {
            return Ok(true);
        }
        total += read;
        if total > MAX_AUDIO {
            return Err(format!("{host} sent more audio than any reply needs"));
        }
        if !on_audio(&buffer[..read]) {
            return Ok(false);
        }
    }
}

pub const ELEVENLABS_URL: &str = "https://api.elevenlabs.io";
pub(crate) const ELEVENLABS_MODEL: &str = "eleven_flash_v2_5";
const ELEVENLABS_FORMAT: &str = "mp3_24000_48";

/// Spoken replies, from ElevenLabs with the user's own key.
pub struct Synthesis {
    keys: Arc<VoiceKeys>,
    /// ElevenLabs' base URL; another only in tests.
    elevenlabs_url: String,
}

impl Synthesis {
    /// One piece of a reply, with the user's own key; false when `on_audio` stopped it.
    fn own(
        &self,
        voice: &str,
        key: &str,
        text: &str,
        on_audio: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<bool, String> {
        static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
        let url = format!(
            "{}/v1/text-to-speech/{voice}/stream?output_format={ELEVENLABS_FORMAT}",
            self.elevenlabs_url
        );
        let name = "ElevenLabs";
        let mut response = AGENT
            .get_or_init(|| crate::speech::agent(TIMEOUT))
            .post(&url)
            .header("xi-api-key", key)
            .header("Accept", "audio/mpeg")
            .header("Content-Type", "application/json")
            .send(
                json!({"text": text, "model_id": ELEVENLABS_MODEL})
                    .to_string()
                    .as_bytes(),
            )
            .map_err(|error| format!("calling {name}: {error}"))?;
        let status = response.status().as_u16();
        if matches!(status, 401 | 403) {
            self.keys.refused(Provider::ElevenLabs, true);
            return Err(format!(
                "{name} refused your own key ({status}); the reply stays text"
            ));
        }
        if status != 200 {
            let answer = response
                .body_mut()
                .with_config()
                .limit(1024)
                .read_to_string()
                .unwrap_or_default();
            return Err(format!(
                "{name} answered {status}: {}",
                answer.trim().chars().take(200).collect::<String>()
            ));
        }
        self.keys.refused(Provider::ElevenLabs, false);
        forward(response.into_body().into_reader(), name, on_audio)
    }
}

impl Synthesizer for Synthesis {
    fn synthesize(
        &self,
        text: &str,
        voice: &str,
        on_audio: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<(), String> {
        let (provider, id) = Provider::of(voice).ok_or("not a voice this computer can speak")?;
        let key = self.keys.key(provider).ok_or("no own key for this voice")?;
        for piece in split(text, MAX_TEXT) {
            if !self.own(id, &key, piece, on_audio)? {
                break;
            }
        }
        Ok(())
    }

    fn speaks(&self, voice: &str) -> bool {
        Provider::of(voice).is_some_and(|(provider, _)| self.keys.key(provider).is_some())
    }
}



/// `text` in pieces of at most `max` UTF-16 code units: at the end of a sentence where one fits, else at a comma or
/// a space, else at a character. Pieces with nothing to say are left out.
pub fn split(text: &str, max: usize) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        let mut units = 0;
        let mut fits = rest.len();
        let (mut sentence, mut pause) = (None, None);
        for (at, character) in rest.char_indices() {
            units += character.len_utf16();
            if units > max {
                fits = at;
                break;
            }
            let after = at + character.len_utf8();
            if matches!(character, '.' | '!' | '?' | '。' | '！' | '？' | '…' | '\n') {
                sentence = Some(after);
            } else if matches!(character, ',' | ';' | ':' | '，' | '；' | '：' | '、' | ' ') {
                pause = Some(after);
            }
        }
        let cut = match fits {
            _ if fits == rest.len() => fits,
            // Not even one character fits: it goes alone rather than never.
            0 => rest.chars().next().map_or(0, char::len_utf8),
            _ => sentence.or(pause).unwrap_or(fits),
        };
        let (piece, after) = rest.split_at(cut);
        if piece.chars().any(char::is_alphanumeric) {
            pieces.push(piece.trim());
        }
        rest = after.trim_start();
    }
    pieces
}

/// Each Character's voice file (`characters/<id>/voice.env`), so a spoken reply is in the very voice its lines
/// were made with: `ENGINE=elevenlabs` voices are ElevenLabs voices, spoken with the user's own key.
pub(crate) const VOICE_FILES: &[(&str, &str)] = &[
    ("ada", include_str!("../../characters/ada/voice.env")),
    ("dave", include_str!("../../characters/dave/voice.env")),
    ("hank", include_str!("../../characters/hank/voice.env")),
    ("kai", include_str!("../../characters/kai/voice.env")),
    ("luna", include_str!("../../characters/luna/voice.env")),
    ("mei", include_str!("../../characters/mei/voice.env")),
    (
        "wanwanxiaohe",
        include_str!("../../characters/wanwanxiaohe/voice.env"),
    ),
];
/// The built-in default Character speaks with ElevenLabs' Jessica (voices/README.md); so does no Character pack, or
/// one these files don't know.
const DEFAULT_VOICE: &str = "elevenlabs:cgSgspJ2msm6clMCkdW9";
/// ElevenLabs' stock Sarah and George, voices of no Character, for a Character with no voice of its own.
const STOCK_FEMALE: &str = "elevenlabs:EXAVITQu4vr4xnSDxMaL";
const STOCK_MALE: &str = "elevenlabs:JBFqnCBsd6RMkjVDRZzb";

/// The name of `character`'s voice (the id the box reports in `VOICES`), as `<vendor>:<voice>`.
/// The model is multilingual, so a reply in another language than the Character's keeps the Character's voice.
/// A Character made in the App ([`crate::making`]) speaks in the voice it was made with.
pub fn voice_for(character: Option<&str>) -> String {
    voice_among(character, crate::making::DIR.get().map(PathBuf::as_path))
}

/// [`voice_for`], with the Characters made in `made`.
fn voice_among(character: Option<&str>, made: Option<&Path>) -> String {
    let Some(file) =
        character.and_then(|character| VOICE_FILES.iter().find(|(id, _)| *id == character))
    else {
        return character
            .zip(made)
            .and_then(|(id, made)| crate::making::voice_of(made, id))
            .unwrap_or_else(|| DEFAULT_VOICE.to_owned());
    };
    let field = |key: &str| voice_field(file.1, key);
    match (field("ENGINE"), field("VOICE")) {
        (Some("elevenlabs"), Some(voice)) => format!("elevenlabs:{voice}"),
        _ if field("GENDER") == Some("male") => STOCK_MALE.to_owned(),
        _ => STOCK_FEMALE.to_owned(),
    }
}

/// One `KEY=value` of a `voice.env`.
pub(crate) fn voice_field<'a>(file: &'a str, key: &str) -> Option<&'a str> {
    file.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
}

/// The synthesizer: the user's own keys.
pub fn from_env(keys: Arc<VoiceKeys>) -> Arc<dyn Synthesizer> {
    Arc::new(Synthesis {
        keys,
        elevenlabs_url: ELEVENLABS_URL.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::TcpListener;

    use super::*;

    #[test]
    fn each_character_speaks_in_the_voice_of_its_lines() {
        assert_eq!(voice_for(Some("ada")), "elevenlabs:tcSsCJFYrsHpK5rCQZMf");
        assert_eq!(voice_for(Some("hank")), "elevenlabs:U6jH23hcPh0zH17nf3DT");
        assert_eq!(
            voice_for(Some("wanwanxiaohe")),
            STOCK_FEMALE,
            "a stock voice for a Character with none of its own"
        );
        assert_eq!(voice_for(Some("robot")), DEFAULT_VOICE);
        assert_eq!(voice_for(None), DEFAULT_VOICE, "no Character pack");
        assert_eq!(
            voice_for(Some("hsiaoyu")),
            DEFAULT_VOICE,
            "an old voice pack"
        );
    }

    #[test]
    fn a_made_character_speaks_in_the_voice_it_was_made_with() {
        let made = crate::voice_keys::tests::scratch("made-voice");
        std::fs::create_dir_all(made.join("owl-1a2b3c4d")).unwrap();
        let manifest = r#"{"name":"Owl","base":"ada","voice":"hank","lines":[{"occasion":"done","text":"Done."}]}"#;
        std::fs::write(made.join("owl-1a2b3c4d/manifest.json"), manifest).unwrap();
        assert_eq!(
            voice_among(Some("owl-1a2b3c4d"), Some(&made)),
            "elevenlabs:U6jH23hcPh0zH17nf3DT"
        );
        assert_eq!(
            voice_among(Some("ada"), Some(&made)),
            "elevenlabs:tcSsCJFYrsHpK5rCQZMf",
            "a preset stays a preset"
        );
        assert_eq!(
            voice_among(Some("owl-0000"), Some(&made)),
            DEFAULT_VOICE,
            "not made here"
        );
        assert_eq!(
            voice_among(Some("../owl-1a2b3c4d"), Some(&made)),
            DEFAULT_VOICE
        );
        assert_eq!(voice_among(Some("owl-1a2b3c4d"), None), DEFAULT_VOICE);
        let _ = std::fs::remove_dir_all(made);
    }

    #[test]
    fn long_text_is_split_at_sentences_then_pauses_then_characters() {
        assert_eq!(split("  Short one.  ", 1000), ["Short one."]);
        assert_eq!(split("One. Two! Three?", 10), ["One. Two!", "Three?"]);
        assert_eq!(
            split("第一句。第二句。第三句。", 8),
            ["第一句。第二句。", "第三句。"]
        );
        assert_eq!(split("one two three four", 9), ["one two", "three", "four"]);
        assert_eq!(split("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        // A character outside the BMP is units.
        assert_eq!(split("𠀀𠀀𠀀", 4), ["𠀀𠀀", "𠀀"]);
        assert!(split("😀", 4).is_empty(), "nothing to say");
        assert!(split(" ... ", 10).is_empty(), "nothing to say");
        let long = "This sentence is about forty characters. ".repeat(60);
        let pieces = split(&long, MAX_TEXT);
        assert!(pieces.len() > 1);
        assert!(
            pieces
                .iter()
                .all(|piece| piece.encode_utf16().count() <= MAX_TEXT && piece.ends_with('.'))
        );
    }

    /// Serves one canned HTTP answer on a local port; returns the base URL and the request it got.
    fn serve(answer: Vec<u8>) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            while !String::from_utf8_lossy(&request).contains("}") {
                let read = socket.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..read]);
            }
            socket.write_all(&answer).unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        (base, handle)
    }

    fn chunked(pieces: &[&[u8]], finished: bool) -> Vec<u8> {
        let mut out =
            b"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nTransfer-Encoding: chunked\r\n\r\n"
                .to_vec();
        for piece in pieces {
            out.extend_from_slice(format!("{:x}\r\n", piece.len()).as_bytes());
            out.extend_from_slice(piece);
            out.extend_from_slice(b"\r\n");
        }
        if finished {
            out.extend_from_slice(b"0\r\n\r\n");
        }
        out
    }

    fn own_keys(keys: &[(Provider, &str)]) -> Arc<VoiceKeys> {
        let keys_file = Arc::new(VoiceKeys::new(Some(&crate::voice_keys::tests::scratch(
            "synthesis",
        ))));
        let changes: Vec<_> = keys
            .iter()
            .map(|(provider, key)| (*provider, Some((*key).to_owned())))
            .collect();
        keys_file.set(&changes).unwrap();
        keys_file
    }

    fn synthesis(keys: Arc<VoiceKeys>, elevenlabs: &str) -> Synthesis {
        Synthesis {
            keys,
            elevenlabs_url: elevenlabs.to_owned(),
        }
    }

    fn speak_with(synthesis: &Synthesis, voice: &str) -> (Result<(), String>, Vec<u8>) {
        let mut audio = Vec::new();
        let result = synthesis.synthesize("你好。", voice, &mut |piece| {
            audio.extend_from_slice(piece);
            true
        });
        (result, audio)
    }

    #[test]
    fn an_own_key_speaks_its_voices_and_nothing_else_is_tried() {
        let keys = own_keys(&[(Provider::ElevenLabs, "sk_own_eleven")]);
        let (eleven, eleven_server) = serve(chunked(&[b"\xff\xf3own"], true));
        let synthesis = synthesis(keys, &eleven);

        assert_eq!(
            speak_with(&synthesis, "elevenlabs:voice1"),
            (Ok(()), b"\xff\xf3own".to_vec())
        );
        let request = eleven_server.join().unwrap();
        assert!(
            request.starts_with(
                "POST /v1/text-to-speech/voice1/stream?output_format=mp3_24000_48 HTTP/1.1\r\n"
            ),
            "{request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("xi-api-key: sk_own_eleven")
        );
        assert!(
            request.contains(r#""model_id":"eleven_flash_v2_5""#)
                && request.contains(r#""text":"你好。""#),
            "{request}"
        );

        assert!(!synthesis.speaks("en-US-JennyNeural"));
        assert!(speak_with(&synthesis, "en-US-JennyNeural").0.is_err());
    }

    #[test]
    fn without_an_own_key_nothing_is_spoken() {
        // Nothing listens on port 9: a reply sent there would fail another way.
        let synthesis = synthesis(own_keys(&[]), "http://127.0.0.1:9");
        assert!(!synthesis.speaks("elevenlabs:voice1"));
        assert_eq!(
            speak_with(&synthesis, "elevenlabs:voice1").0,
            Err("no own key for this voice".to_owned())
        );
    }

    #[test]
    fn a_refused_own_key_is_reported_and_the_reply_stays_silent() {
        let keys = own_keys(&[(Provider::ElevenLabs, "sk_bad")]);
        let (eleven, server) = serve(
            b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"
                .to_vec(),
        );
        let synthesis = synthesis(keys.clone(), &eleven);
        let (result, audio) = speak_with(&synthesis, "elevenlabs:voice1");
        server.join().unwrap();
        let error = result.unwrap_err();
        assert!(
            error.contains("ElevenLabs refused your own key (401)") && !error.contains("sk_bad"),
            "{error}"
        );
        assert!(audio.is_empty());
        assert_eq!(
            keys.status().elevenlabs,
            crate::voice_keys::KeyStatus {
                set: true,
                refused: true
            }
        );
    }

    #[test]
    fn a_stream_cut_short_or_an_error_status_is_a_failure() {
        // The connection drops before the chunked body's end: what came is kept, but it failed.
        let keys = own_keys(&[(Provider::ElevenLabs, "sk_own")]);
        let (base, server) = serve(chunked(&[b"ID3abc"], false));
        let (result, audio) = speak_with(&synthesis(keys.clone(), &base), "elevenlabs:voice1");
        server.join().unwrap();
        assert!(result.is_err(), "{result:?}");
        assert_eq!(audio, b"ID3abc");

        let (base, server) = serve(
            b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 15\r\n\r\nupstream failed".to_vec(),
        );
        let (result, audio) = speak_with(&synthesis(keys, &base), "elevenlabs:voice1");
        server.join().unwrap();
        assert_eq!(
            result.unwrap_err(),
            "ElevenLabs answered 502: upstream failed"
        );
        assert!(audio.is_empty());
    }
}
