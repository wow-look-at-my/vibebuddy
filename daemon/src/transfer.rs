//! Sending a firmware image or a Character pack to the box over any link, in chunks, resumably (ADR-0016,
//! docs/protocol.md "Firmware and Character packs over the link"). The image goes as `xfer.chunk` lines of at most [`CHUNK_BYTES`], up to [`WINDOW`]
//! of them unacknowledged; the box acknowledges each with the next offset it expects. When the link drops or
//! moves (the Relay to the cable, say), the daemon waits for it to come back and asks again with `xfer.begin`,
//! and the box answers with where it got to. A box the Relay reports absent counts as a dropped link. A box that
//! forgot the transfer (it restarted) answers `sequence`, and the daemon asks again the same way.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;
use vibebuddy_protocol::Event;

use crate::serial_transport::{DeviceMessage, Transport};
use crate::voice_writer::send;

/// Image bytes per chunk: as base64 inside its line about 4.2 KB, under the box's 4608.
pub const CHUNK_BYTES: usize = 3072;
/// Chunks sent ahead of the acknowledgements: enough to keep the Relay busy at 130-250 ms a round trip and the box
/// writing; [`PACE`] keeps the rate down.
const WINDOW: usize = 16;
/// At most one chunk this often, 40 a second: under the Relay's 50 messages a second for good, with room for the
/// heartbeat (docs/relay-protocol.md). About 120 KB/s, more than the box takes over any link.
const PACE: Duration = Duration::from_millis(25);
/// The box erases what the image needs before it answers `xfer.begin`: up to 4 MB, a block per main-loop pass.
const READY_TIMEOUT: Duration = Duration::from_secs(60);
/// No answer to a chunk for this long means it went missing: ask where the box got to.
const STEP_TIMEOUT: Duration = Duration::from_secs(20);
/// The box reads 4 MB back to check it before it answers `xfer.end`.
const END_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a dropped link may take to come back before the update gives up.
const RECONNECT_TIMEOUT: Duration = Duration::from_secs(180);
/// Times the transfer may start over from where the box got to.
const RESUMES: u32 = 10;

/// Why sending stopped.
#[derive(Debug, PartialEq, Eq)]
enum Stop {
    /// The link moved, or the box went quiet: resume from where it got to.
    Resume,
    /// The link dropped, or the Relay says the box is absent: wait for it to come back, then resume.
    Gone,
    /// The box answered `sequence`: it has no transfer, say after a restart. Ask again too; but answers like this
    /// one that come before `xfer.ready` are for chunks sent before the `xfer.begin`, and change nothing.
    Lost,
    /// The box refused, with its code; or something that resuming won't fix.
    Fail(String),
}

fn event(name: &str, fields: serde_json::Value) -> Event {
    let mut event = Event::named(name);
    if let serde_json::Value::Object(map) = fields {
        event.extra = map.into_iter().collect();
    }
    event
}

/// What goes over: `firmware` (the app image) or `character` (a Character pack).
pub fn begin_event(kind: &str, size: usize, sha256: &str) -> Event {
    event(
        "xfer.begin",
        serde_json::json!({"kind": kind, "size": size, "sha256": sha256}),
    )
}

/// `round` comes back in the box's `xfer.ack`: it tells answers to chunks sent again from those sent before.
pub fn chunk_event(offset: usize, round: u64, piece: &[u8]) -> Event {
    event(
        "xfer.chunk",
        serde_json::json!({
            "offset": offset,
            "round": round,
            "crc": crc32fast::hash(piece),
            "data": base64::engine::general_purpose::STANDARD.encode(piece),
        }),
    )
}

/// The box's next answer about the transfer: `xfer.ready`/`xfer.ack` with an offset, `xfer.done`, or a stop.
async fn answer(
    bus: &mut broadcast::Receiver<DeviceMessage>,
    timeout: Duration,
) -> Result<(String, Option<usize>), Stop> {
    answer_event(bus, timeout).await.map(|event| {
        let offset = event
            .extra
            .get("offset")
            .and_then(serde_json::Value::as_u64)
            .map(|offset| offset as usize);
        (event.event, offset)
    })
}

/// [`answer`], the whole event.
async fn answer_event(
    bus: &mut broadcast::Receiver<DeviceMessage>,
    timeout: Duration,
) -> Result<Event, Stop> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let message = match tokio::time::timeout_at(deadline, bus.recv()).await {
            Ok(Ok(message)) => message,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) => {
                return Err(Stop::Fail("device message channel closed".to_owned()));
            }
            Err(_) => return Err(Stop::Resume),
        };
        match message {
            DeviceMessage::Disconnected => return Err(Stop::Gone),
            DeviceMessage::Connected { .. }
            | DeviceMessage::ConnectedNetwork { .. }
            | DeviceMessage::ConnectedRelay { .. } => {
                return Err(Stop::Resume);
            }
            DeviceMessage::Event(event) if event.event == "xfer.error" => {
                let code = event
                    .extra
                    .get("code")
                    .and_then(|value| value.as_str())
                    .unwrap_or("error");
                // A long K2 on the box (ADR-0016).
                if code == "cancelled" {
                    return Err(Stop::Fail("cancelled on the box".to_owned()));
                }
                if code == "sequence" {
                    return Err(Stop::Lost);
                }
                return Err(Stop::Fail(format!("the box refused the image: {code}")));
            }
            DeviceMessage::Event(event) if event.event.starts_with("xfer.") => return Ok(event),
            _ => {}
        }
    }
}

/// Waits for the link to come back after it dropped; true once it has.
async fn reconnected(bus: &mut broadcast::Receiver<DeviceMessage>) -> bool {
    let deadline = tokio::time::Instant::now() + RECONNECT_TIMEOUT;
    loop {
        match tokio::time::timeout_at(deadline, bus.recv()).await {
            Ok(Ok(
                DeviceMessage::Connected { .. }
                | DeviceMessage::ConnectedNetwork { .. }
                | DeviceMessage::ConnectedRelay { .. },
            )) => return true,
            Ok(Ok(_) | Err(broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => return false,
        }
    }
}

/// Sends the image from wherever the box says it got to, until the box has it all.
async fn send_from_box(
    transport: &Arc<dyn Transport>,
    bus: &mut broadcast::Receiver<DeviceMessage>,
    kind: &str,
    image: &[u8],
    sha256: &str,
    round: &mut u64,
    progress: &impl Fn(f32),
) -> Result<(), Stop> {
    // A new round for each start, so answers to chunks of an earlier one are told apart.
    *round += 1;
    send(transport, begin_event(kind, image.len(), sha256))
        .await
        .map_err(|_| Stop::Resume)?;
    let mut acked = loop {
        match answer(bus, READY_TIMEOUT).await {
            Ok((name, Some(offset))) if name == "xfer.ready" => break offset,
            Ok(_) | Err(Stop::Lost) => {}
            Err(stop) => return Err(stop),
        }
    };
    let mut next = acked;
    let mut pace = tokio::time::interval(PACE);
    pace.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    while acked < image.len() {
        while next < image.len() && next < acked + WINDOW * CHUNK_BYTES {
            pace.tick().await;
            let end = (next + CHUNK_BYTES).min(image.len());
            send(transport, chunk_event(next, *round, &image[next..end]))
                .await
                .map_err(|_| Stop::Resume)?;
            next = end;
        }
        let ack = answer_event(bus, STEP_TIMEOUT).await?;
        let number = |key: &str| ack.extra.get(key).and_then(serde_json::Value::as_u64);
        if ack.event != "xfer.ack" {
            continue;
        }
        let Some(offset) = number("offset").map(|offset| offset as usize) else {
            continue;
        };
        if offset > acked {
            acked = offset;
            progress(acked as f32 / image.len() as f32);
        } else if offset == acked && next > acked && number("round") == Some(*round) {
            // The box still wants `acked` after a later chunk of this round: that one was lost on the way, a line
            // damaged on the UART bridge say. Send again from there now, not after STEP_TIMEOUT and a resume. The
            // other chunks of this round that were still on their way answer the same, but with the old round.
            next = acked;
            *round += 1;
        }
    }
    Ok(())
}

/// Sends the whole image of `kind` and has the box check and commit it: firmware boots next and the box restarts; a
/// pack becomes the Character. Returns the box's `xfer.done`, which names the new Character for a pack. `progress`
/// receives 0 to 1 as the box acknowledges, and `resuming` is told when the transfer waits for the link.
pub async fn send_image(
    transport: Arc<dyn Transport>,
    mut bus: broadcast::Receiver<DeviceMessage>,
    kind: &str,
    image: Vec<u8>,
    progress: impl Fn(f32),
    resuming: impl Fn(),
) -> Result<Event, String> {
    let sha256: String = Sha256::digest(&image)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let mut resumes = 0;
    let mut round = 0;
    loop {
        let stop = match send_from_box(
            &transport, &mut bus, kind, &image, &sha256, &mut round, &progress,
        )
        .await
        {
            Ok(()) => {
                send(&transport, Event::named("xfer.end")).await?;
                // Answers to chunks sent twice may still be on their way.
                let mut end = answer_event(&mut bus, END_TIMEOUT).await;
                while matches!(&end, Ok(ack) if ack.event == "xfer.ack") {
                    end = answer_event(&mut bus, END_TIMEOUT).await;
                }
                match end {
                    Ok(done) if done.event == "xfer.done" => return Ok(done),
                    Ok(other) => return Err(format!("unexpected {} at the end", other.event)),
                    // The box keeps what it has: asking again carries on, and ends again.
                    Err(Stop::Fail(why)) => return Err(why),
                    Err(stop) => stop,
                }
            }
            Err(Stop::Fail(why)) => return Err(why),
            Err(stop) => stop,
        };
        resumes += 1;
        if resumes > RESUMES {
            return Err(if stop == Stop::Lost {
                "the box kept losing its place in the image"
            } else {
                "the link kept dropping"
            }
            .to_owned());
        }
        resuming();
        // A link that moved is already back; one that dropped gets a while to come back. A box that dropped away (it
        // lost power, say, and the Relay says it is absent) won't answer until it is back.
        let connected = stop != Stop::Gone && transport_connected(&mut bus).await;
        if !connected && !reconnected(&mut bus).await {
            return Err("the box didn't come back".to_owned());
        }
    }
}

/// Drains what the bus already holds; true if the latest word about the link is that it is up.
async fn transport_connected(bus: &mut broadcast::Receiver<DeviceMessage>) -> bool {
    let mut connected = None;
    while let Ok(message) = bus.try_recv() {
        match message {
            DeviceMessage::Disconnected => connected = Some(false),
            DeviceMessage::Connected { .. }
            | DeviceMessage::ConnectedNetwork { .. }
            | DeviceMessage::ConnectedRelay { .. } => connected = Some(true),
            _ => {}
        }
    }
    connected.unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use crate::serial_transport::TransportError;

    /// Plays the box: takes chunks in order, acknowledges each with the next offset, and on xfer.begin
    /// answers where it got to. `drop_after` chunks in, it drops the link once and loses what was in flight.
    /// `forget_after` chunks in, it restarts once without the link noticing: it forgets the transfer and answers
    /// `sequence` until the next xfer.begin. `damage_after` chunks in, one chunk arrives damaged: the box drops it
    /// with no answer about the transfer, and acknowledges the chunks after it with the offset it still wants.
    /// `vanish_after` chunks in, it loses power while the daemon moves to the Relay: the link comes up, and the
    /// Relay's word that the box is absent comes only after the next xfer.begin. The box stays silent until `away`
    /// is cleared.
    struct FakeBox {
        bus: broadcast::Sender<DeviceMessage>,
        state: Mutex<BoxState>,
    }

    #[derive(Default)]
    struct BoxState {
        next: usize,
        written: Vec<u8>,
        drop_after: Option<usize>,
        forget_after: Option<usize>,
        damage_after: Option<usize>,
        forgotten: bool,
        vanish_after: Option<usize>,
        away: bool,
        begins_while_away: usize,
        forget_always: bool,
        /// Chunks the box was sent, copies too.
        chunks: usize,
        /// Answers to chunks of the link before it moved, reaching the daemon right after `xfer.ready`; they carry an
        /// old round.
        stale_acks: usize,
    }

    impl Transport for FakeBox {
        fn send(&self, frame: Vec<u8>) -> Result<(), TransportError> {
            let value: serde_json::Value = serde_json::from_slice(&frame).unwrap();
            let reply = |name: &str, offset: Option<usize>| {
                let mut event = Event::named(name);
                if let Some(offset) = offset {
                    event.extra.insert("offset".to_owned(), offset.into());
                }
                let _ = self.bus.send(DeviceMessage::Event(event));
            };
            let sequence = || {
                let mut event = Event::named("xfer.error");
                event.extra.insert("code".to_owned(), "sequence".into());
                let _ = self.bus.send(DeviceMessage::Event(event));
            };
            let mut state = self.state.lock().unwrap();
            let BoxState {
                next,
                written,
                drop_after,
                forget_after,
                damage_after,
                forgotten,
                forget_always,
                chunks,
                stale_acks,
                vanish_after,
                away,
                begins_while_away,
            } = &mut *state;
            if *away {
                if value["event"] == "xfer.begin" {
                    *begins_while_away += 1;
                    if *begins_while_away == 1 {
                        let _ = self.bus.send(DeviceMessage::Disconnected);
                    }
                }
                return Ok(());
            }
            match value["event"].as_str().unwrap() {
                "xfer.begin" => {
                    *forgotten = false;
                    reply("xfer.ready", Some(*next));
                    for _ in 0..std::mem::take(stale_acks) {
                        let mut event = Event::named("xfer.ack");
                        event.extra.insert("offset".to_owned(), (*next).into());
                        event.extra.insert("round".to_owned(), 0.into());
                        let _ = self.bus.send(DeviceMessage::Event(event));
                    }
                }
                "xfer.chunk" => {
                    *chunks += 1;
                    if *forget_after == Some(0) {
                        *forget_after = None;
                        *forgotten = true;
                        *next = 0;
                        written.clear();
                    }
                    if *forgotten || *forget_always {
                        sequence();
                        return Ok(());
                    }
                    if let Some(count) = forget_after {
                        *count -= 1;
                    }
                    if *drop_after == Some(0) {
                        *drop_after = None;
                        let _ = self.bus.send(DeviceMessage::Disconnected);
                        let _ = self.bus.send(DeviceMessage::ConnectedRelay {
                            url: "wss://relay".to_owned(),
                        });
                        return Ok(());
                    }
                    if let Some(count) = drop_after {
                        *count -= 1;
                    }
                    if *vanish_after == Some(0) {
                        *vanish_after = None;
                        *away = true;
                        let _ = self.bus.send(DeviceMessage::ConnectedRelay {
                            url: "wss://relay".to_owned(),
                        });
                        return Ok(());
                    }
                    if let Some(count) = vanish_after {
                        *count -= 1;
                    }
                    if *damage_after == Some(0) {
                        // The real box answers `ERROR invalid_json`, which isn't about the transfer.
                        *damage_after = None;
                        return Ok(());
                    }
                    if let Some(count) = damage_after {
                        *count -= 1;
                    }
                    let offset = value["offset"].as_u64().unwrap() as usize;
                    if offset == *next {
                        let data = base64::engine::general_purpose::STANDARD
                            .decode(value["data"].as_str().unwrap())
                            .unwrap();
                        assert_eq!(
                            value["crc"].as_u64().unwrap() as u32,
                            crc32fast::hash(&data)
                        );
                        written.extend_from_slice(&data);
                        *next += data.len();
                    }
                    let mut event = Event::named("xfer.ack");
                    event.extra.insert("offset".to_owned(), (*next).into());
                    event
                        .extra
                        .insert("round".to_owned(), value["round"].clone());
                    let _ = self.bus.send(DeviceMessage::Event(event));
                }
                "xfer.end" if *forgotten => sequence(),
                "xfer.end" => reply("xfer.done", None),
                other => panic!("unexpected {other}"),
            }
            Ok(())
        }
    }

    async fn run(image: Vec<u8>, state: BoxState) -> (Result<(), String>, Vec<u8>, usize) {
        let (bus, receiver) = broadcast::channel(1024);
        run_on(image, state, bus, receiver).await
    }

    async fn run_on(
        image: Vec<u8>,
        state: BoxState,
        bus: broadcast::Sender<DeviceMessage>,
        receiver: broadcast::Receiver<DeviceMessage>,
    ) -> (Result<(), String>, Vec<u8>, usize) {
        let fake = Arc::new(FakeBox {
            bus,
            state: Mutex::new(state),
        });
        let resumed = Arc::new(Mutex::new(0));
        let counter = resumed.clone();
        let result = send_image(
            fake.clone(),
            receiver,
            "firmware",
            image,
            |_| {},
            move || *counter.lock().unwrap() += 1,
        )
        .await
        .map(|_| ());
        let written = std::mem::take(&mut fake.state.lock().unwrap().written);
        let resumed = *resumed.lock().unwrap();
        (result, written, resumed)
    }

    #[tokio::test]
    async fn the_whole_image_arrives_in_order() {
        let image: Vec<u8> = (0..20_000).map(|index| (index * 31) as u8).collect();
        let (result, written, resumed) = run(image.clone(), BoxState::default()).await;
        assert_eq!(result, Ok(()));
        assert_eq!(written, image);
        assert_eq!(resumed, 0);
    }

    #[tokio::test]
    async fn a_dropped_link_resumes_where_the_box_got_to() {
        let image: Vec<u8> = (0..30_000).map(|index| (index * 17) as u8).collect();
        let (result, written, resumed) = run(
            image.clone(),
            BoxState {
                drop_after: Some(3),
                ..BoxState::default()
            },
        )
        .await;
        assert_eq!(result, Ok(()));
        assert_eq!(written, image, "nothing written twice, nothing missing");
        assert_eq!(resumed, 1);
    }

    #[tokio::test]
    async fn a_box_that_forgot_the_transfer_starts_over() {
        let image: Vec<u8> = (0..30_000).map(|index| (index * 13) as u8).collect();
        let (result, written, resumed) = run(
            image.clone(),
            BoxState {
                forget_after: Some(3),
                ..BoxState::default()
            },
        )
        .await;
        assert_eq!(result, Ok(()));
        assert_eq!(written, image);
        assert_eq!(resumed, 1);
    }

    #[tokio::test]
    async fn a_damaged_chunk_is_sent_again_without_resuming() {
        let image: Vec<u8> = (0..60_000).map(|index| (index * 11) as u8).collect();
        let started = std::time::Instant::now();
        let (result, written, resumed) = run(
            image.clone(),
            BoxState {
                damage_after: Some(3),
                ..BoxState::default()
            },
        )
        .await;
        assert_eq!(result, Ok(()));
        assert_eq!(written, image);
        assert_eq!(resumed, 0, "sent again at once, not after STEP_TIMEOUT");
        assert!(started.elapsed() < STEP_TIMEOUT / 2);
    }

    #[tokio::test]
    async fn stale_acks_and_a_damaged_chunk_send_little_twice() {
        let image: Vec<u8> = (0..300_000).map(|index| (index * 5) as u8).collect();
        let (bus, receiver) = broadcast::channel(4096);
        let state = BoxState {
            damage_after: Some(40),
            stale_acks: WINDOW,
            ..BoxState::default()
        };
        let fake = Arc::new(FakeBox {
            bus,
            state: Mutex::new(state),
        });
        let result = send_image(
            fake.clone(),
            receiver,
            "firmware",
            image.clone(),
            |_| {},
            || {},
        )
        .await
        .map(|_| ());
        let state = fake.state.lock().unwrap();
        assert_eq!(result, Ok(()));
        assert_eq!(state.written, image);
        let needed = image.len().div_ceil(CHUNK_BYTES);
        assert!(
            state.chunks <= needed + 2 * WINDOW,
            "{} chunks sent for {needed}",
            state.chunks
        );
    }

    #[tokio::test]
    async fn a_box_that_keeps_forgetting_says_so() {
        let image: Vec<u8> = (0..10_000).map(|index| index as u8).collect();
        let (result, _, resumed) = run(
            image,
            BoxState {
                forget_always: true,
                ..BoxState::default()
            },
        )
        .await;
        assert_eq!(
            result,
            Err("the box kept losing its place in the image".to_owned())
        );
        assert_eq!(resumed, RESUMES as usize);
    }

    #[tokio::test]
    async fn stale_sequence_errors_dont_fail_the_next_transfer() {
        let image: Vec<u8> = (0..20_000).map(|index| (index * 7) as u8).collect();
        let (bus, receiver) = broadcast::channel(1024);
        // Chunks of an earlier transfer, still reaching a box that has forgotten it.
        for _ in 0..WINDOW {
            let mut event = Event::named("xfer.error");
            event.extra.insert("code".to_owned(), "sequence".into());
            bus.send(DeviceMessage::Event(event)).unwrap();
        }
        let (result, written, resumed) =
            run_on(image.clone(), BoxState::default(), bus, receiver).await;
        assert_eq!(result, Ok(()));
        assert_eq!(written, image);
        assert_eq!(resumed, 0);
    }

    /// The box vanishes partway through and comes back `back_after` later, if at all.
    async fn run_away(
        image: Vec<u8>,
        back_after: Option<Duration>,
    ) -> (Result<(), String>, Vec<u8>, usize, usize) {
        let (bus, receiver) = broadcast::channel(1024);
        let state = BoxState {
            vanish_after: Some(3),
            ..BoxState::default()
        };
        let fake = Arc::new(FakeBox {
            bus: bus.clone(),
            state: Mutex::new(state),
        });
        if let Some(back_after) = back_after {
            let fake = fake.clone();
            tokio::spawn(async move {
                tokio::time::sleep(back_after).await;
                fake.state.lock().unwrap().away = false;
                let _ = fake.bus.send(DeviceMessage::ConnectedRelay {
                    url: "wss://relay".to_owned(),
                });
            });
        }
        let resumed = Arc::new(Mutex::new(0));
        let counter = resumed.clone();
        let result = send_image(
            fake.clone(),
            receiver,
            "firmware",
            image,
            |_| {},
            move || *counter.lock().unwrap() += 1,
        )
        .await
        .map(|_| ());
        let mut state = fake.state.lock().unwrap();
        let written = std::mem::take(&mut state.written);
        let resumed = *resumed.lock().unwrap();
        (result, written, resumed, state.begins_while_away)
    }

    #[tokio::test(start_paused = true)]
    async fn a_box_absent_from_the_relay_is_waited_for() {
        let image: Vec<u8> = (0..30_000).map(|index| (index * 11) as u8).collect();
        let (result, written, resumed, begins_while_away) =
            run_away(image.clone(), Some(Duration::from_secs(150))).await;
        assert_eq!(result, Ok(()));
        assert_eq!(written, image);
        assert_eq!(
            begins_while_away, 1,
            "no asking a box the Relay says is absent"
        );
        assert_eq!(resumed, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_box_that_stays_absent_from_the_relay_fails_the_transfer() {
        let image: Vec<u8> = (0..30_000).map(|index| (index * 5) as u8).collect();
        let started = tokio::time::Instant::now();
        let (result, _, _, begins_while_away) = run_away(image, None).await;
        assert_eq!(result, Err("the box didn't come back".to_owned()));
        assert_eq!(begins_while_away, 1);
        assert!(
            started.elapsed() <= RECONNECT_TIMEOUT + Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_full_chunk_fits_the_boxs_line_limit() {
        let frame = chunk_event(4_000_000, u64::from(u32::MAX), &vec![0xAB; CHUNK_BYTES])
            .to_ndjson()
            .unwrap();
        assert!(frame.len() <= 4608, "{} bytes", frame.len());
    }
}
