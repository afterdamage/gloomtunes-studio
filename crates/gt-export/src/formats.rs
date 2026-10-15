//! Compressed export formats (Prompt 26, ARCHITECTURE.md D135): FLAC, Ogg Vorbis and MP3,
//! with the project's title, artist and genre as tags.
//!
//! - FLAC: `flacenc` (Apache-2.0, pure Rust). Lossless at 16 or 24 bits; the samples are the
//!   same integers (and the same dither) as a WAV export of the same render.
//! - Ogg Vorbis: `vorbis_rs` (BSD-3-Clause), which builds the aoTuV/Lancer libvorbis sources.
//!   Quality-based VBR, 0 to 10 like `oggenc -q`.
//! - MP3: `mp3lame-encoder` (LGPL-3.0, compatible with our GPL-3.0), which builds LAME from
//!   source. CBR at a fixed bitrate or VBR at a quality (0 best to 9), with a LAME/Xing header so
//!   players know the length and the encoder delay.
//!
//! Every writer goes through a `.part` file and renames it, like the WAV writer.

use std::num::{NonZeroU32, NonZeroU8};
use std::path::Path;

use crate::{quantize, BitDepth, ExportError, ExportSettings, Tpdf};

/// File format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    /// PCM WAV.
    #[default]
    Wav,
    /// FLAC (lossless).
    Flac,
    /// Ogg Vorbis.
    Ogg,
    /// MP3.
    Mp3,
}

impl Format {
    /// Every format, in menu order.
    pub const ALL: [Format; 4] = [Self::Wav, Self::Flac, Self::Ogg, Self::Mp3];

    /// File extension.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Flac => "flac",
            Self::Ogg => "ogg",
            Self::Mp3 => "mp3",
        }
    }

    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Wav => "WAV",
            Self::Flac => "FLAC",
            Self::Ogg => "Ogg Vorbis",
            Self::Mp3 => "MP3",
        }
    }

    /// The format a file name asks for, by extension.
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|f| f.extension() == ext || (ext == "oga" && *f == Self::Ogg))
    }

    /// True for the lossy formats.
    pub fn lossy(self) -> bool {
        matches!(self, Self::Ogg | Self::Mp3)
    }
}

/// MP3 bitrates offered for CBR, in kbit/s.
pub const MP3_BITRATES: [u16; 8] = [64, 96, 128, 160, 192, 224, 256, 320];

/// MP3 rate control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mp3Mode {
    /// Constant bitrate in kbit/s (one of [`MP3_BITRATES`]).
    Cbr(u16),
    /// Variable bitrate at a quality, 0 (best, about 245 kbit/s) to 9 (about 65 kbit/s).
    Vbr(u8),
}

impl Default for Mp3Mode {
    fn default() -> Self {
        Self::Cbr(320)
    }
}

/// Tags written into FLAC, Ogg and MP3 files (empty ones are left out).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tags {
    /// Track title.
    pub title: String,
    /// Artist.
    pub artist: String,
    /// Genre.
    pub genre: String,
}

impl Tags {
    /// From the project info.
    pub fn of(info: &gt_core::ProjectInfo) -> Self {
        Self {
            title: info.title.trim().to_owned(),
            artist: info.artist.trim().to_owned(),
            genre: info.genre.trim().to_owned(),
        }
    }

    /// `(Vorbis comment name, value)` pairs that are set.
    pub fn pairs(&self) -> Vec<(&'static str, &str)> {
        [
            ("TITLE", self.title.as_str()),
            ("ARTIST", self.artist.as_str()),
            ("GENRE", self.genre.as_str()),
        ]
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .collect()
    }
}

const VENDOR: &str = "GloomTunes Studio";

/// Writes interleaved stereo `audio` in `settings.format`. `seed` varies the dither between
/// the files of one export.
pub fn write_audio(
    path: &Path,
    audio: &[f32],
    settings: &ExportSettings,
    seed: u64,
    tags: &Tags,
) -> Result<(), ExportError> {
    let bytes = match settings.format {
        Format::Wav => return crate::write_wav(path, audio, settings, seed),
        Format::Flac => flac_bytes(audio, settings, seed, tags)?,
        Format::Ogg => ogg_bytes(audio, settings.sample_rate, settings.ogg_quality, tags)?,
        Format::Mp3 => mp3_bytes(audio, settings.sample_rate, settings.mp3, tags)?,
    };
    let tmp = path.with_extension(format!("{}.part", settings.format.extension()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

fn codec_error(what: &str, e: impl std::fmt::Display) -> ExportError {
    ExportError::Io(std::io::Error::other(format!("{what}: {e}")))
}

/// The integers a 16 or 24-bit export writes (32-bit float is written as 24-bit FLAC).
pub fn pcm_samples(audio: &[f32], settings: &ExportSettings, seed: u64) -> (Vec<i32>, u16) {
    let bits = match settings.depth {
        BitDepth::Pcm16 => 16,
        BitDepth::Pcm24 | BitDepth::Float32 => 24,
    };
    let dither = settings.dither && settings.depth != BitDepth::Float32;
    let mut tpdf = Tpdf::new(seed);
    let pcm = audio
        .iter()
        .map(|&x| quantize(x, bits, if dither { tpdf.next() } else { 0.0 }))
        .collect();
    (pcm, bits)
}

/// A FLAC VORBIS_COMMENT block body: little-endian lengths, vendor, then `NAME=value` items.
pub fn vorbis_comment(tags: &Tags) -> Vec<u8> {
    let mut out = Vec::new();
    let put = |s: &[u8], out: &mut Vec<u8>| {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s);
    };
    put(VENDOR.as_bytes(), &mut out);
    let pairs = tags.pairs();
    out.extend_from_slice(&(pairs.len() as u32).to_le_bytes());
    for (k, v) in pairs {
        put(format!("{k}={v}").as_bytes(), &mut out);
    }
    out
}

pub(crate) fn flac_bytes(
    audio: &[f32],
    settings: &ExportSettings,
    seed: u64,
    tags: &Tags,
) -> Result<Vec<u8>, ExportError> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;

    let (pcm, bits) = pcm_samples(audio, settings, seed);
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, e)| codec_error("FLAC settings", e))?;
    let source = flacenc::source::MemSource::from_samples(
        &pcm,
        2,
        usize::from(bits),
        settings.sample_rate as usize,
    );
    let mut stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| codec_error("FLAC", e))?;
    let block = flacenc::component::MetadataBlockData::new_unknown(4, &vorbis_comment(tags))
        .map_err(|e| codec_error("FLAC tags", e))?;
    stream.add_metadata_block(block);
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| codec_error("FLAC", e))?;
    let mut bytes = sink.as_slice().to_vec();
    // flacenc counts the short last block in the minimum block size; the format says it must
    // not (decoders such as symphonia then take the fixed-size stream for a variable one).
    if bytes.len() >= 12 && &bytes[..4] == b"fLaC" {
        bytes.copy_within(10..12, 8);
    }
    Ok(bytes)
}

/// Ogg Vorbis at `quality` 0 to 10 (`oggenc -q`; libvorbis takes 0 to 1).
fn ogg_bytes(audio: &[f32], rate: u32, quality: f32, tags: &Tags) -> Result<Vec<u8>, ExportError> {
    use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoderBuilder};

    let err = |e| codec_error("Ogg Vorbis", e);
    let mut builder = VorbisEncoderBuilder::new_with_serial(
        NonZeroU32::new(rate).ok_or(ExportError::Empty)?,
        NonZeroU8::new(2).expect("two channels"),
        Vec::new(),
        // A fixed stream serial, so the same render gives the same bytes.
        0x676c_6d74,
    );
    builder.bitrate_management_strategy(VorbisBitrateManagementStrategy::QualityVbr {
        target_quality: (quality / 10.0).clamp(-0.1, 1.0),
    });
    builder.comment_tags(tags.pairs()).map_err(err)?;
    let mut enc = builder.build().map_err(err)?;
    let mut planes = [Vec::with_capacity(1024), Vec::with_capacity(1024)];
    for block in audio.chunks(2048) {
        for p in &mut planes {
            p.clear();
        }
        for f in block.chunks_exact(2) {
            planes[0].push(f[0]);
            planes[1].push(f[1]);
        }
        enc.encode_audio_block(&planes).map_err(err)?;
    }
    enc.finish().map_err(err)
}

fn mp3_bytes(audio: &[f32], rate: u32, mode: Mp3Mode, tags: &Tags) -> Result<Vec<u8>, ExportError> {
    use mp3lame_encoder::{Bitrate, Builder, FlushGap, InterleavedPcm, Quality, VbrMode};

    let err = |e: &dyn std::fmt::Display| codec_error("MP3", e);
    let mut b = Builder::new().ok_or_else(|| err(&"LAME could not start"))?;
    b.set_num_channels(2).map_err(|e| err(&e))?;
    b.set_sample_rate(rate).map_err(|e| err(&e))?;
    b.set_quality(Quality::Best).map_err(|e| err(&e))?;
    match mode {
        Mp3Mode::Cbr(kbps) => {
            let br = match kbps {
                0..=64 => Bitrate::Kbps64,
                65..=96 => Bitrate::Kbps96,
                97..=128 => Bitrate::Kbps128,
                129..=160 => Bitrate::Kbps160,
                161..=192 => Bitrate::Kbps192,
                193..=224 => Bitrate::Kbps224,
                225..=256 => Bitrate::Kbps256,
                _ => Bitrate::Kbps320,
            };
            b.set_vbr_mode(VbrMode::Off).map_err(|e| err(&e))?;
            b.set_brate(br).map_err(|e| err(&e))?;
        }
        Mp3Mode::Vbr(q) => {
            b.set_vbr_mode(VbrMode::Mtrh).map_err(|e| err(&e))?;
            b.set_vbr_quality(lame_quality(q)).map_err(|e| err(&e))?;
        }
    }
    b.set_to_write_vbr_tag(true).map_err(|e| err(&e))?;
    let mut enc = b.build().map_err(|e| err(&e))?;
    let mut data = Vec::new();
    // LAME needs 1.25 bytes per sample plus 7200 bytes of headroom per call.
    for block in audio.chunks(2 * 8192) {
        data.reserve(block.len() * 5 / 8 + 7200);
        enc.encode_to_vec(InterleavedPcm(block), &mut data)
            .map_err(|e| err(&e))?;
    }
    data.reserve(7200);
    enc.flush_to_vec::<FlushGap>(&mut data)
        .map_err(|e| err(&e))?;
    // The first frame was a placeholder for the LAME/Xing header (length, delay, padding).
    let mut tag = Vec::with_capacity(enc.lame_tag_size().max(1));
    if enc.lame_tag_encode_to_vec(&mut tag).is_some() && tag.len() <= data.len() {
        data[..tag.len()].copy_from_slice(&tag);
    }
    let mut out = id3v2(tags);
    out.extend_from_slice(&data);
    Ok(out)
}

fn lame_quality(q: u8) -> mp3lame_encoder::Quality {
    use mp3lame_encoder::Quality::*;
    [
        Best,
        SecondBest,
        NearBest,
        VeryNice,
        Nice,
        Good,
        Decent,
        Ok,
        SecondWorst,
        Worst,
    ][usize::from(q.min(9))]
}

/// An ID3v2.4 tag with UTF-8 text frames (TIT2 title, TPE1 artist, TCON genre); empty when no
/// tag is set.
pub fn id3v2(tags: &Tags) -> Vec<u8> {
    let frames: Vec<(&[u8; 4], &str)> = [
        (b"TIT2", tags.title.as_str()),
        (b"TPE1", tags.artist.as_str()),
        (b"TCON", tags.genre.as_str()),
    ]
    .into_iter()
    .filter(|(_, v)| !v.is_empty())
    .collect();
    if frames.is_empty() {
        return Vec::new();
    }
    let mut body = Vec::new();
    for (id, text) in frames {
        body.extend_from_slice(id);
        // Encoding byte 3 = UTF-8.
        body.extend_from_slice(&synchsafe(text.len() as u32 + 1));
        body.extend_from_slice(&[0, 0, 3]);
        body.extend_from_slice(text.as_bytes());
    }
    let mut out = b"ID3\x04\x00\x00".to_vec();
    out.extend_from_slice(&synchsafe(body.len() as u32));
    out.extend_from_slice(&body);
    out
}

/// A 28-bit size in four 7-bit bytes.
fn synchsafe(n: u32) -> [u8; 4] {
    [
        (n >> 21 & 0x7f) as u8,
        (n >> 14 & 0x7f) as u8,
        (n >> 7 & 0x7f) as u8,
        (n & 0x7f) as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_follow_the_extension() {
        assert_eq!(Format::from_path(Path::new("a/b.FLAC")), Some(Format::Flac));
        assert_eq!(Format::from_path(Path::new("x.oga")), Some(Format::Ogg));
        assert_eq!(Format::from_path(Path::new("x.aiff")), None);
        assert!(Format::Mp3.lossy() && !Format::Flac.lossy());
    }

    #[test]
    fn tags_are_encoded() {
        let t = Tags {
            title: "Night Shift".into(),
            artist: String::new(),
            genre: "Darkwave".into(),
        };
        let c = vorbis_comment(&t);
        assert_eq!(&c[..4], &(VENDOR.len() as u32).to_le_bytes());
        assert_eq!(c[4 + VENDOR.len()], 2, "two comments");
        assert!(c.windows(16).any(|w| w == b"TITLE=Night Shif"));
        let id3 = id3v2(&t);
        assert_eq!(&id3[..5], b"ID3\x04\x00");
        let size = id3[6..10]
            .iter()
            .fold(0usize, |n, &b| n << 7 | usize::from(b));
        assert_eq!(size, id3.len() - 10);
        assert!(id3.windows(4).any(|w| w == b"TCON"));
        assert!(!id3.windows(4).any(|w| w == b"TPE1"));
        assert!(id3v2(&Tags::default()).is_empty());
        assert_eq!(synchsafe(200), [0, 0, 1, 72]);
    }
}
