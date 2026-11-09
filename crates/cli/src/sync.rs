//! One sync: open the session, drain history into the store, acknowledging each chunk.

use std::fmt::Write as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use futures::{Stream, StreamExt};
use healthstrap_core::command::{self, Outcome, Response, number};
use healthstrap_core::frame::{Frame, encode_command};
use healthstrap_core::offload::{Offload, Step};
use healthstrap_core::reassembler::{Output, Reassembler};

use crate::ble::Link;
use crate::store::Store;

/// How long to wait for a command's final reply.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// How long history may go quiet before the attempt is treated as finished.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// What one sync achieved.
#[derive(Debug, Default)]
pub struct Summary {
    /// Chunks saved and acknowledged.
    pub chunks: usize,
    /// Frames received, including ones the store already had.
    pub frames: usize,
    /// Frames that were new to the store.
    pub new_frames: usize,
    /// Whether the strap said it had nothing more to send.
    pub complete: bool,
}

/// Runs the session and offload on an open link.
pub async fn run(link: &Link, store: &mut Store, device: &str) -> Result<Summary> {
    let mut frames = Box::pin(frames(link.notifications().await?));
    let mut sequence = Sequence::default();

    link.send(&encode_command(sequence.next(), number::GET_HELLO, &[1]))
        .await?;
    await_reply(&mut frames, number::GET_HELLO, sequence.last()).await?;

    let (seconds, ticks) = now_for_strap()?;
    link.send(&command::set_clock(sequence.next(), seconds, ticks))
        .await?;
    await_reply(&mut frames, number::SET_CLOCK, sequence.last()).await?;

    link.send(&encode_command(
        sequence.next(),
        number::GET_DATA_RANGE,
        &[],
    ))
    .await?;
    let range = await_reply(&mut frames, number::GET_DATA_RANGE, sequence.last()).await?;
    // The range layout is not decoded yet; log it raw so it can be matched against cursors.
    println!("strap data range: {}", hex(&range));

    link.send(&command::send_historical_data(sequence.next()))
        .await?;

    let mut offload = Offload::new();
    let mut summary = Summary::default();
    loop {
        let Ok(next) = tokio::time::timeout(IDLE_TIMEOUT, frames.next()).await else {
            break;
        };
        let Some(frame) = next else {
            bail!("strap disconnected during sync")
        };
        match offload.on_frame(frame) {
            Step::Continue => {}
            Step::ChunkReady {
                frames: chunk,
                token,
            } => {
                summary.new_frames +=
                    store.commit_chunk(device, now_ms()?, &chunk, Some(&token))?;
                link.send(&command::acknowledge_chunk(sequence.next(), &token))
                    .await?;
                summary.chunks += 1;
                summary.frames += chunk.len();
                println!(
                    "chunk {} acknowledged: {} frames ({} new) so far, trim cursor {}",
                    summary.chunks,
                    summary.frames,
                    summary.new_frames,
                    token.trim_cursor()
                );
            }
            Step::Complete { unacknowledged } => {
                summary.new_frames +=
                    store.commit_chunk(device, now_ms()?, &unacknowledged, None)?;
                summary.frames += unacknowledged.len();
                summary.complete = true;
                break;
            }
        }
    }
    Ok(summary)
}

/// Turns raw notifications into checked frames. Rejected bytes are reported and skipped.
fn frames(notifications: impl Stream<Item = Vec<u8>>) -> impl Stream<Item = Frame> {
    let mut reassembler = Reassembler::new();
    notifications
        .flat_map(move |bytes| futures::stream::iter(reassembler.push(&bytes)))
        .filter_map(|output| async move {
            match output {
                Output::Frame(frame) => Some(frame),
                Output::Rejected { error, bytes } => {
                    eprintln!(
                        "skipped {} bytes that failed checks: {error:?}",
                        bytes.len()
                    );
                    None
                }
            }
        })
}

/// Waits for the final reply to a request and returns its body. Pending replies are skipped.
async fn await_reply(
    frames: &mut (impl Stream<Item = Frame> + Unpin),
    command: u8,
    request_sequence: u8,
) -> Result<Vec<u8>> {
    let wait = async {
        while let Some(frame) = frames.next().await {
            let Some(reply) = Response::parse(&frame) else {
                continue;
            };
            if reply.command != command || reply.request_sequence != request_sequence {
                continue;
            }
            match reply.outcome {
                Outcome::Pending => {}
                Outcome::Success => return Ok(reply.body.to_vec()),
                Outcome::Failure | Outcome::Unsupported | Outcome::Unknown(_) => {
                    bail!("command {command} failed: {:?}", reply.outcome);
                }
            }
        }
        bail!("strap disconnected waiting for command {command}")
    };
    tokio::time::timeout(REPLY_TIMEOUT, wait)
        .await
        .with_context(|| format!("no reply to command {command}"))?
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// The 8-bit request counter. It wraps, which the strap expects.
#[derive(Default)]
struct Sequence(u8);

impl Sequence {
    const fn next(&mut self) -> u8 {
        self.0 = self.0.wrapping_add(1);
        self.0
    }

    const fn last(&self) -> u8 {
        self.0
    }
}

/// Current time as Unix seconds plus 1/32768-second ticks.
fn now_for_strap() -> Result<(u32, u16)> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?;
    let seconds = u32::try_from(now.as_secs())?;
    let ticks = u16::try_from(u64::from(now.subsec_nanos()) * 32_768 / 1_000_000_000)?;
    Ok((seconds, ticks))
}

fn now_ms() -> Result<i64> {
    Ok(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}
