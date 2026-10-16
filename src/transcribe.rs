//! Speech-to-text backends. New code, not part of the Haversine library.
//!
//! The crate bundles no model. Transcription goes to one of two backends, so any model works:
//! - [`Backend::OpenAi`]: any server with an OpenAI-compatible `POST /v1/audio/transcriptions`
//!   endpoint (vLLM, whisper.cpp `whisper-server`, speaches, a cloud API, ...).
//! - [`Backend::Local`] (feature `local`): an ONNX model folder run in-process via
//!   `transcribe-rs`, the engine behind the Handy app; Parakeet, Canary, Cohere, SenseVoice and
//!   GigaAM. Models Handy or Spokenly already downloaded work as-is.
//! - [`Backend::Command`]: any program that takes a WAV path as its last argument and prints
//!   the transcript to stdout (run through `sh -c`; the path is `"$1"`).
//!
//! Audio is resampled from the ring's ~10 kHz to 16 kHz mono 16-bit PCM first, the rate most
//! speech models expect.

use std::path::Path;

/// Target sample rate sent to every backend.
pub const TARGET_RATE: u32 = 16_000;

#[derive(Debug, thiserror::Error)]
pub enum TranscribeError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("server returned {status}: {body}")]
    Server { status: u16, body: String },
    #[error("command failed ({status}): {stderr}")]
    Command { status: String, stderr: String },
    #[error("local model: {0}")]
    Local(String),
    #[error("unsupported WAV: {0}")]
    Wav(String),
}

#[derive(Debug, Clone)]
pub enum Backend {
    OpenAi {
        /// Base URL, e.g. `http://localhost:8000/v1`.
        url: String,
        model: String,
        api_key: Option<String>,
        language: Option<String>,
        prompt: Option<String>,
    },
    /// ONNX model directory, run in-process.
    #[cfg(feature = "local")]
    Local(std::sync::Arc<std::sync::Mutex<local::LocalModel>>),
    /// Shell command; the WAV path is passed as `"$1"` (appended when the command doesn't use it).
    Command(String),
}

impl Backend {
    /// Transcribe a 16 kHz mono 16-bit WAV file.
    pub async fn transcribe_file(&self, wav16k: &Path) -> Result<String, TranscribeError> {
        match self {
            Backend::OpenAi {
                url,
                model,
                api_key,
                language,
                prompt,
            } => {
                let bytes = tokio::fs::read(wav16k).await?;
                let part = reqwest::multipart::Part::bytes(bytes)
                    .file_name("audio.wav")
                    .mime_str("audio/wav")?;
                let mut form = reqwest::multipart::Form::new()
                    .part("file", part)
                    .text("model", model.clone())
                    .text("response_format", "json");
                if let Some(l) = language {
                    form = form.text("language", l.clone());
                }
                if let Some(p) = prompt {
                    form = form.text("prompt", p.clone());
                }
                let endpoint = format!("{}/audio/transcriptions", url.trim_end_matches('/'));
                let mut req = reqwest::Client::new().post(endpoint).multipart(form);
                if let Some(k) = api_key {
                    req = req.bearer_auth(k);
                }
                let resp = req.send().await?;
                let status = resp.status();
                let body = resp.text().await?;
                if !status.is_success() {
                    return Err(TranscribeError::Server {
                        status: status.as_u16(),
                        body,
                    });
                }
                // {"text": "..."}; fall back to the raw body for servers that ignore response_format.
                Ok(serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("text").and_then(|t| t.as_str()).map(str::to_owned))
                    .unwrap_or(body)
                    .trim()
                    .to_owned())
            }
            #[cfg(feature = "local")]
            Backend::Local(m) => {
                let m = m.clone();
                let path = wav16k.to_owned();
                tokio::task::spawn_blocking(move || m.lock().unwrap().transcribe_file(&path))
                    .await
                    .map_err(|e| TranscribeError::Local(e.to_string()))?
            }
            Backend::Command(cmd) => {
                let script = if cmd.contains("$1") {
                    cmd.clone()
                } else {
                    format!("{cmd} \"$1\"")
                };
                let out = tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(script)
                    .arg("index01")
                    .arg(wav16k)
                    .output()
                    .await?;
                if !out.status.success() {
                    return Err(TranscribeError::Command {
                        status: out.status.to_string(),
                        stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
                    });
                }
                Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
            }
        }
    }

    /// Resample `wav` (any rate, mono 16-bit) to 16 kHz next to it (`*.16k.wav`) and transcribe.
    pub async fn transcribe_wav(&self, wav: &Path) -> Result<String, TranscribeError> {
        let (rate, samples) = read_wav_mono16(&std::fs::read(wav)?)?;
        let out = wav.with_extension("16k.wav");
        let resampled = resample(&samples, rate, TARGET_RATE);
        let as_u16: Vec<u16> = resampled.iter().map(|&s| s as u16).collect();
        // Already DC-free; wav_bytes' DC removal is then a no-op to within rounding.
        std::fs::write(&out, crate::wav::wav_bytes(&as_u16, TARGET_RATE))?;
        let text = self.transcribe_file(&out).await;
        let _ = std::fs::remove_file(&out);
        text
    }
}

#[cfg(feature = "local")]
pub mod local {
    use super::TranscribeError;
    use std::path::{Path, PathBuf};
    use transcribe_rs::onnx::Quantization;
    use transcribe_rs::{SpeechModel, TranscribeOptions};

    /// Where other apps keep their downloaded models; accepted as `--model-dir handy` etc.
    pub fn app_model_root(name: &str) -> Option<PathBuf> {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
        let id = match name {
            "handy" => "com.pais.handy",
            "spokenly" => "app.spokenly.linux",
            _ => return None,
        };
        Some(base.join(id).join("models"))
    }

    /// Resolve a `--model-dir` spec to a model directory:
    /// - `auto`: search Handy's then Spokenly's downloads, see [`pick`];
    /// - `handy` / `spokenly`: the same, in that app only;
    /// - `handy/<folder>` / `spokenly/<folder>`: that model;
    /// - anything else: a path.
    pub fn resolve_dir(spec: &str) -> Result<PathBuf, TranscribeError> {
        let (app, rest) = spec.split_once('/').unwrap_or((spec, ""));
        let apps: &[&str] = match app {
            "auto" => &["handy", "spokenly"],
            "handy" => &["handy"],
            "spokenly" => &["spokenly"],
            _ => return Ok(PathBuf::from(spec)),
        };
        if !rest.is_empty() {
            return Ok(app_model_root(app).unwrap().join(rest));
        }
        let mut dirs = Vec::new();
        for root in apps.iter().filter_map(|a| app_model_root(a)) {
            if let Ok(rd) = std::fs::read_dir(&root) {
                let mut d: Vec<PathBuf> = rd
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.is_dir() && detect(p).is_ok())
                    .collect();
                d.sort();
                dirs.extend(d);
            }
        }
        pick(&dirs).ok_or_else(|| {
            TranscribeError::Local(format!("no usable models found for --model-dir {spec}"))
        })
    }

    /// Preference: Parakeet v2 (English; most reliable on short ring clips), then any Parakeet,
    /// then the first model found.
    fn pick(dirs: &[PathBuf]) -> Option<PathBuf> {
        let named = |s: &str| dirs.iter().find(|p| p.to_string_lossy().contains(s));
        named("parakeet-tdt-0.6b-v2")
            .or_else(|| named("parakeet"))
            .or(dirs.first())
            .cloned()
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Engine {
        Parakeet,
        Canary,
        Cohere,
        SenseVoice,
        GigaAm,
    }

    /// Guess the engine and quantization from the file names in `dir`.
    pub fn detect(dir: &Path) -> Result<(Engine, Quantization), TranscribeError> {
        let names: Vec<String> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with("._")) // macOS resource forks shipped in some archives
            .collect();
        let has = |pre: &str| {
            names
                .iter()
                .any(|n| n.starts_with(pre) && n.contains(".onnx"))
        };
        let engine = if has("decoder_joint-model") {
            Engine::Parakeet
        } else if has("decoder-model") && has("encoder-model") {
            Engine::Canary
        } else if has("cohere-encoder") {
            Engine::Cohere
        } else if has("model") && names.iter().any(|n| n == "tokens.txt") {
            Engine::SenseVoice
        } else if has("model") && names.iter().any(|n| n == "vocab.txt") {
            Engine::GigaAm
        } else {
            return Err(TranscribeError::Local(format!(
                "don't recognise the model in {}",
                dir.display()
            )));
        };
        let q = |s: &str| names.iter().any(|n| n.contains(&format!(".{s}.onnx")));
        let quant = if q("int8") {
            Quantization::Int8
        } else if q("int4") {
            Quantization::Int4
        } else if q("fp16") {
            Quantization::FP16
        } else {
            Quantization::FP32
        };
        Ok((engine, quant))
    }

    pub struct LocalModel {
        model: Box<dyn SpeechModel>,
        options: TranscribeOptions,
        pub engine: Engine,
        pub dir: PathBuf,
    }

    impl std::fmt::Debug for LocalModel {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("LocalModel")
                .field("engine", &self.engine)
                .field("dir", &self.dir)
                .finish()
        }
    }

    impl LocalModel {
        pub fn load(spec: &str, language: Option<String>) -> Result<Self, TranscribeError> {
            use transcribe_rs::onnx::*;
            let dir = resolve_dir(spec)?;
            let (engine, q) = detect(&dir)?;
            let e = |e: transcribe_rs::TranscribeError| TranscribeError::Local(e.to_string());
            let model: Box<dyn SpeechModel> = match engine {
                Engine::Parakeet => Box::new(parakeet::ParakeetModel::load(&dir, &q).map_err(e)?),
                Engine::Canary => Box::new(canary::CanaryModel::load(&dir, &q).map_err(e)?),
                Engine::Cohere => Box::new(cohere::CohereModel::load(&dir, &q).map_err(e)?),
                Engine::SenseVoice => {
                    Box::new(sense_voice::SenseVoiceModel::load(&dir, &q).map_err(e)?)
                }
                Engine::GigaAm => Box::new(gigaam::GigaAMModel::load(&dir, &q).map_err(e)?),
            };
            Ok(Self {
                model,
                options: TranscribeOptions {
                    language,
                    ..Default::default()
                },
                engine,
                dir,
            })
        }

        pub fn transcribe_file(&mut self, wav16k: &Path) -> Result<String, TranscribeError> {
            let (_, samples) = super::read_wav_mono16(&std::fs::read(wav16k)?)?;
            let f: Vec<f32> = samples.iter().map(|&s| s as f32 / 32768.0).collect();
            self.model
                .transcribe(&f, &self.options)
                .map(|r| r.text.trim().to_owned())
                .map_err(|e| TranscribeError::Local(e.to_string()))
        }
    }
}

/// Parse a PCM 16-bit mono WAV. Returns (rate, samples).
pub fn read_wav_mono16(b: &[u8]) -> Result<(u32, Vec<i16>), TranscribeError> {
    let bad = |m: &str| TranscribeError::Wav(m.into());
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err(bad("not RIFF/WAVE"));
    }
    let (mut i, mut rate, mut data) = (12usize, None, None);
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let len = u32::from_le_bytes(b[i + 4..i + 8].try_into().unwrap()) as usize;
        let body = &b[i + 8..(i + 8 + len).min(b.len())];
        if id == b"fmt " && body.len() >= 16 {
            let fmt = u16::from_le_bytes([body[0], body[1]]);
            let ch = u16::from_le_bytes([body[2], body[3]]);
            let bits = u16::from_le_bytes([body[14], body[15]]);
            if fmt != 1 || ch != 1 || bits != 16 {
                return Err(bad("need PCM 16-bit mono"));
            }
            rate = Some(u32::from_le_bytes(body[4..8].try_into().unwrap()));
        } else if id == b"data" {
            data = Some(body);
        }
        i += 8 + len + (len & 1);
    }
    let (rate, data) = (
        rate.ok_or_else(|| bad("no fmt"))?,
        data.ok_or_else(|| bad("no data"))?,
    );
    Ok((
        rate,
        data.chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect(),
    ))
}

/// Windowed-sinc (Lanczos, a = 8) resampler. Low-passes at the lower Nyquist when downsampling.
pub fn resample(x: &[i16], from: u32, to: u32) -> Vec<i16> {
    if from == to || x.is_empty() {
        return x.to_vec();
    }
    const A: f64 = 8.0;
    let ratio = to as f64 / from as f64;
    let cutoff = ratio.min(1.0); // relative to the input rate
    let n_out = ((x.len() as f64) * ratio).round() as usize;
    let sinc = |t: f64| {
        if t == 0.0 {
            1.0
        } else {
            (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t)
        }
    };
    let half = (A / cutoff).ceil() as isize;
    (0..n_out)
        .map(|j| {
            let center = j as f64 / ratio;
            let c = center.floor() as isize;
            let (mut acc, mut wsum) = (0.0, 0.0);
            for k in (c - half + 1)..=(c + half) {
                if k < 0 || k as usize >= x.len() {
                    continue;
                }
                let t = (center - k as f64) * cutoff;
                if t.abs() >= A {
                    continue;
                }
                let w = sinc(t) * sinc(t / A);
                acc += x[k as usize] as f64 * w;
                wsum += w;
            }
            let v = if wsum != 0.0 { acc / wsum } else { 0.0 };
            v.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_preserves_tone_and_length() {
        let from = 9997;
        let x: Vec<i16> = (0..from)
            .map(|i| {
                ((i as f64 * 2.0 * std::f64::consts::PI * 440.0 / from as f64).sin() * 10000.0)
                    as i16
            })
            .collect();
        let y = resample(&x, from, TARGET_RATE);
        assert_eq!(y.len(), TARGET_RATE as usize);
        // Compare against the ideal 16 kHz tone away from the edges.
        for (j, &v) in y.iter().enumerate().skip(100).take(15000) {
            let ideal = (j as f64 * 2.0 * std::f64::consts::PI * 440.0 / TARGET_RATE as f64).sin()
                * 10000.0;
            assert!((v as f64 - ideal).abs() < 60.0, "j={j} v={v} ideal={ideal}");
        }
    }

    #[test]
    fn wav_roundtrip() {
        let s: Vec<u16> = vec![0, 100, 65436, 5];
        let (rate, back) = read_wav_mono16(&crate::wav::wav_bytes(&s, 9997)).unwrap();
        assert_eq!(rate, 9997);
        assert_eq!(back.len(), 4);
    }
}
