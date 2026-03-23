use opendeck_types::{DecodeError, Decoder, TrackInfo};
use rtrb::{Producer, RingBuffer};
use std::path::Path;

pub mod symphonia_decoder;
pub use symphonia_decoder::SymphoniaDecoder;

/// Ring buffer capacity: 4 seconds stereo at 44.1 kHz.
pub const RING_CAPACITY: usize = 44_100 * 2 * 4;

/// Commands from the audio engine to the decode thread.
pub enum DecodeCmd {
    Load(TrackInfo),
    Seek(u64),
    Eject,
}

/// Events from the decode thread back to the audio engine.
pub enum DecodeEvent {
    SeekComplete { actual_sample: u64 },
    BufferUnderrun,
}

/// Spawns the decode thread.  Returns the PCM ring consumer and command channels.
pub fn spawn(
    cmd_rx: std::sync::mpsc::Receiver<DecodeCmd>,
    event_tx: rtrb::Producer<DecodeEvent>,
) -> (rtrb::Consumer<f32>, std::thread::JoinHandle<()>) {
    let (mut pcm_tx, pcm_rx) = RingBuffer::<f32>::new(RING_CAPACITY);

    let handle = std::thread::Builder::new()
        .name("opendeck-decode".into())
        .spawn(move || {
            decode_loop(cmd_rx, event_tx, &mut pcm_tx);
        })
        .expect("failed to spawn decode thread");

    (pcm_rx, handle)
}

fn decode_loop(
    cmd_rx: std::sync::mpsc::Receiver<DecodeCmd>,
    mut event_tx: rtrb::Producer<DecodeEvent>,
    pcm_tx: &mut Producer<f32>,
) {
    let mut decoder: Option<Box<dyn Decoder>> = None;
    let mut decode_buf = vec![0f32; 4096];

    loop {
        // Non-blocking check for commands.
        match cmd_rx.try_recv() {
            Ok(DecodeCmd::Load(track)) => {
                match SymphoniaDecoder::open(&track.path) {
                    Ok(d) => decoder = Some(Box::new(d)),
                    Err(e) => log::error!("failed to open {:?}: {}", track.path, e),
                }
            }
            Ok(DecodeCmd::Seek(sample)) => {
                if let Some(d) = &mut decoder {
                    match d.seek(sample) {
                        Ok(actual) => {
                            // Flush the PCM ring — stale data is now invalid.
                            // (rtrb doesn't have a flush; we skip ahead by draining.)
                            let _ = event_tx.push(DecodeEvent::SeekComplete { actual_sample: actual });
                        }
                        Err(e) => log::warn!("seek failed: {}", e),
                    }
                }
            }
            Ok(DecodeCmd::Eject) => {
                decoder = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
        }

        // Fill the ring buffer with decoded audio.
        if let Some(d) = &mut decoder {
            let slots = pcm_tx.slots();
            if slots > decode_buf.len() {
                match d.decode(&mut decode_buf) {
                    Ok(0) => {
                        // EOF — wait for next command.
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Ok(frames) => {
                        let samples = frames * 2;
                        if let Ok(chunk) = pcm_tx.write_chunk_uninit(samples) {
                            chunk.fill_from_iter(
                                decode_buf[..samples].iter().copied()
                            );
                        }
                    }
                    Err(e) => {
                        log::error!("decode error: {}", e);
                        decoder = None;
                    }
                }
            } else {
                // Ring buffer is full — yield and let the audio thread consume.
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        } else {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

/// Open the best available decoder for the given path.
pub fn open_decoder(path: &Path) -> Result<Box<dyn Decoder>, DecodeError> {
    Ok(Box::new(SymphoniaDecoder::open(path)?))
}
