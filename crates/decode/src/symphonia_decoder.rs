use opendeck_types::{DecodeError, Decoder};
use std::path::Path;
use symphonia::core::{
    audio::SampleBuffer,
    codecs::DecoderOptions,
    formats::{FormatOptions, SeekMode, SeekTo},
    io::MediaSourceStream,
    meta::MetadataOptions,
    probe::Hint,
    units::Time,
};

pub struct SymphoniaDecoder {
    format:      Box<dyn symphonia::core::formats::FormatReader>,
    decoder:     Box<dyn symphonia::core::codecs::Decoder>,
    track_id:    u32,
    sample_rate: u32,
    channels:    u8,
    total_frames: Option<u64>,
    sample_buf:  Option<SampleBuffer<f32>>,
}

impl SymphoniaDecoder {
    pub fn open(path: &Path) -> Result<Self, DecodeError> {
        let file = std::fs::File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        let meta_opts = MetadataOptions::default();
        let fmt_opts = FormatOptions { enable_gapless: true, ..Default::default() };

        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &fmt_opts, &meta_opts)
            .map_err(|e| DecodeError::UnsupportedFormat(e.to_string()))?;

        let format = probed.format;
        let track = format.default_track()
            .ok_or_else(|| DecodeError::UnsupportedFormat("no default track".into()))?;

        let track_id   = track.id;
        let sample_rate = track.codec_params.sample_rate.unwrap_or(44100);
        let channels   = track.codec_params.channels
            .map(|c| c.count() as u8)
            .unwrap_or(2);
        let total_frames = track.codec_params.n_frames;

        let dec_opts = DecoderOptions::default();
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &dec_opts)
            .map_err(|e| DecodeError::Codec(e.to_string()))?;

        Ok(Self {
            format,
            decoder,
            track_id,
            sample_rate,
            channels,
            total_frames,
            sample_buf: None,
        })
    }
}

impl Decoder for SymphoniaDecoder {
    fn decode(&mut self, out: &mut [f32]) -> Result<usize, DecodeError> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(symphonia::core::errors::Error::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(0); // EOF
                }
                Err(e) => return Err(DecodeError::Codec(e.to_string())),
            };

            if packet.track_id() != self.track_id {
                continue;
            }

            let decoded = self.decoder.decode(&packet)
                .map_err(|e| DecodeError::Codec(e.to_string()))?;

            let spec = *decoded.spec();
            let frames = decoded.frames();

            let buf = self.sample_buf.get_or_insert_with(|| {
                SampleBuffer::<f32>::new(frames as u64, spec)
            });
            buf.copy_interleaved_ref(decoded);

            let samples = buf.samples();
            let copy_len = samples.len().min(out.len());
            out[..copy_len].copy_from_slice(&samples[..copy_len]);
            return Ok(copy_len / spec.channels.count());
        }
    }

    fn seek(&mut self, sample: u64) -> Result<u64, DecodeError> {
        let ts = sample as f64 / self.sample_rate as f64;
        self.format.seek(
            SeekMode::Accurate,
            SeekTo::Time { time: Time::from(ts), track_id: Some(self.track_id) },
        )
        .map(|pos| pos.actual_ts)
        .map_err(|e| DecodeError::Codec(e.to_string()))
    }

    fn sample_rate(&self) -> u32 { self.sample_rate }
    fn channels(&self)    -> u8  { self.channels }
    fn total_frames(&self) -> Option<u64> { self.total_frames }
}
