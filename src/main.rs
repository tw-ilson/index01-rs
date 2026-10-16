//! Index 01 CLI. scan / info / list / pull / decode are read-only; setup, set-time and
//! erase write to the ring.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use index01::client::{self, Index01Client};
use index01::collection::parse_collection;
use index01::protocol as p;
use index01::transcribe::Backend;
use index01::wav::write_wav;

#[derive(Parser)]
#[command(
    name = "index01",
    about = "Talk to the Index 01 ring directly over BLE"
)]
struct Cli {
    /// Seconds to scan for the ring before giving up.
    #[arg(long, global = true, default_value_t = 30)]
    scan_timeout: u64,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum BackendKind {
    /// In-process ONNX model (Parakeet, Canary, Cohere, SenseVoice, GigaAM); see --model-dir.
    #[cfg(feature = "local")]
    Local,
    /// OpenAI-compatible POST {url}/audio/transcriptions (vLLM, whisper.cpp server, speaches, ...).
    Openai,
    /// Shell command given the 16 kHz WAV path as "$1"; prints the transcript on stdout.
    Command,
}

/// Speech-to-text backend. Any model works: point it at a server or a script.
#[derive(Args, Clone)]
struct BackendArgs {
    #[cfg_attr(feature = "local", arg(long, value_enum, default_value = "local", env = "INDEX01_BACKEND"))]
    #[cfg_attr(not(feature = "local"), arg(long, value_enum, default_value = "openai", env = "INDEX01_BACKEND"))]
    backend: BackendKind,
    /// For --backend local: a model directory, or reuse other apps' downloads: "auto" (Handy,
    /// then Spokenly; prefers Parakeet v2), "handy", "spokenly", or "handy/<model-folder>".
    #[arg(long, default_value = "auto", env = "INDEX01_MODEL_DIR")]
    model_dir: String,
    /// Base URL of the OpenAI-compatible server.
    #[arg(long, default_value = "http://localhost:8000/v1", env = "INDEX01_URL")]
    url: String,
    /// Model name sent to the server.
    #[arg(long, default_value = "openai/whisper-large-v3-turbo", env = "INDEX01_MODEL")]
    model: String,
    /// Bearer token for the server, if it needs one.
    #[arg(long, env = "INDEX01_API_KEY", hide_env_values = true)]
    api_key: Option<String>,
    /// Language hint (ISO 639-1, e.g. "en").
    #[arg(long, env = "INDEX01_LANGUAGE")]
    language: Option<String>,
    /// Prompt / vocabulary hint passed to the model.
    #[arg(long, env = "INDEX01_PROMPT")]
    prompt: Option<String>,
    /// Command for --backend command, e.g. "python parakeet.py".
    #[arg(long, env = "INDEX01_COMMAND")]
    command: Option<String>,
}

impl BackendArgs {
    fn backend(&self) -> Result<Backend, Box<dyn std::error::Error>> {
        Ok(match self.backend {
            #[cfg(feature = "local")]
            BackendKind::Local => {
                let m = index01::transcribe::local::LocalModel::load(&self.model_dir, self.language.clone())?;
                eprintln!("model: {:?} from {}", m.engine, m.dir.display());
                Backend::Local(std::sync::Arc::new(std::sync::Mutex::new(m)))
            }
            BackendKind::Openai => Backend::OpenAi {
                url: self.url.clone(),
                model: self.model.clone(),
                api_key: self.api_key.clone(),
                language: self.language.clone(),
                prompt: self.prompt.clone(),
            },
            BackendKind::Command => Backend::Command(
                self.command.clone().ok_or("--backend command needs --command")?,
            ),
        })
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// List advertising rings.
    Scan {
        #[arg(long, default_value_t = 10)]
        seconds: u64,
    },
    /// Show platform versions, serial, stored-collection range and application data.
    Info { address: String },
    /// List stored collections.
    List { address: String },
    /// Download every stored collection and decode audio to WAV.
    Pull {
        address: String,
        #[arg(long, default_value = "collections")]
        out: PathBuf,
    },
    /// Re-decode collection_*.bin files in a directory into per-recording WAVs (offline).
    Decode {
        #[arg(default_value = "collections")]
        dir: PathBuf,
    },
    /// Transcribe WAV files (16-bit mono, any rate) and write <name>.txt next to each.
    Transcribe {
        #[arg(required = true)]
        wavs: Vec<PathBuf>,
        #[command(flatten)]
        stt: BackendArgs,
    },
    /// Wait for new recordings, then pull, erase, set the clock and transcribe them. Runs forever.
    Watch {
        address: String,
        /// Each batch goes to <out>/<unix time>/ (recording_N.wav, .txt and the raw .bin parts).
        #[arg(long, default_value = "recordings")]
        out: PathBuf,
        /// Run this shell command per transcript: text on stdin, INDEX01_TEXT / INDEX01_WAV /
        /// INDEX01_TXT in the environment. E.g. 'wl-copy' or 'cat >> ~/notes.md'.
        #[arg(long)]
        exec: Option<String>,
        /// Paste each transcript into the focused window (clipboard + a virtual keypress
        /// via /dev/uinput; works on Wayland including GNOME).
        #[arg(long)]
        paste: bool,
        /// Paste shortcut; terminals usually need ctrl-shift-v.
        #[arg(long, default_value = "ctrl-v")]
        paste_keys: String,
        /// Leave recordings on the ring (default: erase after a complete pull).
        #[arg(long)]
        keep: bool,
        #[command(flatten)]
        stt: BackendArgs,
    },
    /// Write the user record and set the ring clock (what the app does on first setup).
    /// Refuses if a user record is already programmed, unless --force.
    Setup {
        address: String,
        #[arg(long, default_value = "index01-rs")]
        user_id: String,
        /// Required: confirm writing to the ring.
        #[arg(long)]
        yes: bool,
        /// Overwrite an existing user record.
        #[arg(long)]
        force: bool,
    },
    /// Set the ring clock to now (the app's service step). Writes to the ring.
    SetTime { address: String },
    /// Delete every stored collection on the ring (the app's EraseCollectionsOperation).
    /// Pull first: this cannot be undone.
    Erase {
        address: String,
        /// Required: confirm the erase.
        #[arg(long)]
        yes: bool,
        /// Directory from `pull`; every stored collection must be saved there or the erase is refused.
        #[arg(long, default_value = "collections")]
        out: PathBuf,
    },
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn now_s() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as u32)
}

/// Serial bytes are MAC-style, most significant first (the BLE address with the top byte differing).
fn mac(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let scan_timeout = Duration::from_secs(cli.scan_timeout);
    match cli.cmd {
        Cmd::Scan { seconds } => {
            let adapter = client::default_adapter().await?;
            for r in client::scan(&adapter, Duration::from_secs(seconds)).await? {
                let uuids: Vec<String> = r.services.iter().map(|u| u.to_string()).collect();
                println!(
                    "{} {} rssi={} uuids={:?}",
                    r.address,
                    r.name.as_deref().unwrap_or("?"),
                    r.rssi.map_or("?".into(), |v| v.to_string()),
                    uuids
                );
                if let Some(a) = r.advertisement {
                    let flags: Vec<&str> = [
                        (a.in_collection_state, "inCollection"),
                        (a.is_moving, "moving"),
                        (a.has_debug_info, "debug"),
                        (a.needs_servicing, "servicing"),
                        (a.is_failsafe(), "failsafe"),
                    ]
                    .into_iter()
                    .filter_map(|(on, n)| on.then_some(n))
                    .collect();
                    println!(
                        "   fingerprint=0x{:08x} collections={} flags={}",
                        a.cacheable_state_fingerprint,
                        a.collection_count,
                        flags.join(" ")
                    );
                }
            }
        }
        Cmd::Info { address } => {
            let mut c = Index01Client::connect_address(&address, scan_timeout).await?;
            let res = async {
                let pv = c.read_platform_versions().await?;
                let serial = c.read_serial_number().await?;
                let idx = c.read_stored_collection_indexes().await?;
                let app = c.read_app_data(0x100).await?;
                println!("platformVersions: {}", hex(&pv));
                println!("serial: {}", mac(&serial));
                println!(
                    "storedCollections: {} .. {}",
                    idx.range_start, idx.range_end
                );
                println!("appData[0:0x100]: {}", hex(&app));
                Ok::<_, client::ClientError>(())
            }
            .await;
            c.disconnect().await?;
            res?;
        }
        Cmd::List { address } => {
            let mut c = Index01Client::connect_address(&address, scan_timeout).await?;
            let res = c.read_stored_collection_indexes().await;
            c.disconnect().await?;
            let idx = res?;
            println!(
                "stored collection index range: {} .. {}",
                idx.range_start, idx.range_end
            );
            for i in idx.iter() {
                println!(
                    " collection {i} at 0x{:08x}",
                    p::collection_address(i as u32)
                );
            }
        }
        Cmd::Pull { address, out } => {
            std::fs::create_dir_all(&out)?;
            let mut c = Index01Client::connect_address(&address, scan_timeout).await?;
            let res = pull(&mut c, &out).await;
            c.disconnect().await?;
            res?;
        }
        Cmd::Decode { dir } => {
            let mut pieces = Vec::new();
            for e in std::fs::read_dir(&dir)? {
                let path = e?.path();
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if let Some(i) = name
                    .strip_prefix("collection_")
                    .and_then(|n| n.strip_suffix(".bin"))
                    .and_then(|n| n.parse::<u16>().ok())
                {
                    pieces.push((i, std::fs::read(&path)?));
                }
            }
            pieces.sort_by_key(|(i, _)| *i);
            write_recordings(&pieces, &dir)?;
        }
        Cmd::Transcribe { wavs, stt } => {
            let backend = stt.backend()?;
            for wav in wavs {
                let text = backend.transcribe_wav(&wav).await?;
                std::fs::write(wav.with_extension("txt"), format!("{text}\n"))?;
                println!("{}: {text}", wav.display());
            }
        }
        Cmd::Watch { address, out, exec, paste, paste_keys, keep, stt } => {
            let paster = if paste {
                Some(index01::paste::Paster::new(index01::paste::parse_keys(&paste_keys)?).map_err(
                    |e| format!("--paste: can't open /dev/uinput ({e}); see README for permissions"),
                )?)
            } else {
                None
            };
            watch(&address, &out, exec.as_deref(), paster.as_ref(), keep, &stt.backend()?, scan_timeout)
                .await?;
        }
        Cmd::Setup { address, user_id, yes, force } => {
            if !yes {
                return Err("refusing to write the user record without --yes".into());
            }
            let mut c = Index01Client::connect_address(&address, scan_timeout).await?;
            let res = async {
                let cur = c.read_app_data(0x10).await?;
                if cur.iter().any(|&b| b != 0xff) && !force {
                    println!("user record already programmed ({}); use --force to overwrite", hex(&cur));
                    return Ok(());
                }
                let now = now_s();
                c.program_user(&user_id, now).await?;
                c.set_time(now).await?;
                println!(
                    "programmed user {user_id:?} (fingerprint 0x{:08x}), clock set to {now}",
                    index01::appdata::user_fingerprint(&user_id)
                );
                Ok::<_, client::ClientError>(())
            }
            .await;
            c.disconnect().await?;
            res?;
        }
        Cmd::SetTime { address } => {
            let mut c = Index01Client::connect_address(&address, scan_timeout).await?;
            let now = now_s();
            let res = c.set_time(now).await;
            c.disconnect().await?;
            res?;
            println!("clock set to {now}");
        }
        Cmd::Erase { address, yes, out } => {
            if !yes {
                return Err("refusing to erase without --yes (pull first; this cannot be undone)".into());
            }
            let mut c = Index01Client::connect_address(&address, scan_timeout).await?;
            let res = async {
                let before = c.read_stored_collection_indexes().await?;
                println!("before: {} .. {}", before.range_start, before.range_end);
                let missing: Vec<u16> = before
                    .iter()
                    .filter(|i| !out.join(format!("collection_{i}.bin")).exists())
                    .collect();
                if !missing.is_empty() {
                    println!(
                        "refusing: collections {missing:?} not pulled into {} (run pull first)",
                        out.display()
                    );
                    return Ok(());
                }
                c.erase_stored_collection_indexes(true).await?;
                c.set_time(now_s()).await?; // the app services the ring after erasing
                let after = c.read_stored_collection_indexes().await?;
                println!("after:  {} .. {}", after.range_start, after.range_end);
                Ok::<_, client::ClientError>(())
            }
            .await;
            c.disconnect().await?;
            res?;
        }
    }
    Ok(())
}

async fn pull(
    c: &mut Index01Client,
    out: &std::path::Path,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let idx = c.read_stored_collection_indexes().await?;
    println!(
        "stored collection index range: {} .. {}",
        idx.range_start, idx.range_end
    );
    let mut pieces = Vec::new();
    for i in idx.iter() {
        let blob = match c.read_collection(i).await {
            Ok(b) => b,
            Err(e) => {
                println!("collection {i}: failed: {e}");
                continue;
            }
        };
        let path = out.join(format!("collection_{i}.bin"));
        std::fs::write(&path, &blob)?;
        println!("collection {i}: saved {} ({} bytes)", path.display(), blob.len());
        pieces.push((i, blob));
    }
    write_recordings(&pieces, out)
}

/// One recording is split across consecutive collections (multipart record 0x52: start index,
/// is_final). Join their audio in index order and write one WAV per recording, named after the
/// first collection index. A recording missing its final part is still written, with a note.
fn write_recordings(
    pieces: &[(u16, Vec<u8>)],
    out: &std::path::Path,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    struct Rec {
        first: u16,
        rate: u32,
        samples: Vec<u16>,
        buttons: Vec<String>,
        parts: usize,
        complete: bool,
        unix_time: u32,
    }
    let mut recs: Vec<Rec> = Vec::new();
    let mut open = false;
    for (i, blob) in pieces {
        let col = match parse_collection(blob) {
            Ok(c) => c,
            Err(e) => {
                println!("collection {i}: decode failed: {e}");
                open = false;
                continue;
            }
        };
        let a = &col.audio;
        if a.samples.is_empty() {
            println!("collection {i}: no audio");
            continue;
        }
        let rate = if a.sample_rate_hz == 0 { 16000 } else { a.sample_rate_hz };
        // Continue the open recording only if this piece is a non-first part of a multipart set.
        let continues = open && a.is_multipart && a.collection_start_index != *i as u32;
        if !continues {
            recs.push(Rec {
                first: *i,
                rate,
                samples: Vec::new(),
                buttons: Vec::new(),
                parts: 0,
                complete: false,
                unix_time: col.unix_time,
            });
        }
        let r = recs.last_mut().unwrap();
        r.samples.extend_from_slice(&a.samples);
        r.buttons
            .extend(col.button_sequence.iter().map(|b| b.as_str().to_string()));
        r.parts += 1;
        r.complete = !a.is_multipart || a.is_final;
        open = a.is_multipart && !a.is_final;
    }
    let mut paths = Vec::new();
    for r in &recs {
        let wav = out.join(format!("recording_{}.wav", r.first));
        write_wav(&r.samples, r.rate, &wav)?;
        println!(
            "saved {} ({} parts, {:.1} s @ {} Hz, time={}, buttons={}{})",
            wav.display(),
            r.parts,
            r.samples.len() as f64 / r.rate as f64,
            r.rate,
            r.unix_time,
            r.buttons.join(" "),
            if r.complete { "" } else { ", INCOMPLETE: final part missing" }
        );
        paths.push(wav);
    }
    Ok(paths)
}

async fn watch(
    address: &str,
    out: &std::path::Path,
    exec: Option<&str>,
    paster: Option<&index01::paste::Paster>,
    keep: bool,
    backend: &Backend,
    scan_timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let addr = btleplug::api::BDAddr::from_str_delim(address)
        .map_err(|_| format!("invalid address {address:?}"))?;
    let adapter = client::default_adapter().await?;
    // With --keep, collections stay on the ring; only pull ones newer than what we've seen.
    let mut seen_end: u16 = 0;
    println!("watching {address}; press the ring button to record");
    loop {
        // Wait for stored collections, and for the ring to finish recording.
        let adv = client::wait_for_advert(&adapter, addr, |a| {
            !a.in_collection_state && (a.collection_count as u16) > if keep { seen_end } else { 0 }
        })
        .await?;
        println!("ring reports {} stored collection(s)", adv.collection_count);
        let batch = out.join(now_s().to_string());
        std::fs::create_dir_all(&batch)?;
        let res: Result<Vec<PathBuf>, Box<dyn std::error::Error>> = async {
            let mut c = Index01Client::connect_address(address, scan_timeout).await?;
            let r = async {
                let idx = c.read_stored_collection_indexes().await?;
                let mut pieces = Vec::new();
                for i in idx.iter().filter(|&i| !keep || i >= seen_end) {
                    let blob = c.read_collection(i).await?;
                    std::fs::write(batch.join(format!("collection_{i}.bin")), &blob)?;
                    pieces.push((i, blob));
                }
                // Only erase when every stored collection was saved.
                if !keep && pieces.len() == idx.iter().count() {
                    c.erase_stored_collection_indexes(true).await?;
                }
                c.set_time(now_s()).await?;
                seen_end = idx.range_end;
                Ok::<_, Box<dyn std::error::Error>>(pieces)
            }
            .await;
            let _ = c.disconnect().await;
            // Let BlueZ finish tearing the link down before the next scan/connect.
            tokio::time::sleep(Duration::from_secs(3)).await;
            write_recordings(&r?, &batch)
        }
        .await;
        let wavs = match res {
            Ok(w) => w,
            Err(e) => {
                eprintln!("pull failed: {e}; retrying on the next advertisement");
                tokio::time::sleep(Duration::from_secs(10)).await;
                continue;
            }
        };
        for wav in wavs {
            let text = match backend.transcribe_wav(&wav).await {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("transcription failed for {}: {e} (audio kept)", wav.display());
                    continue;
                }
            };
            let txt = wav.with_extension("txt");
            std::fs::write(&txt, format!("{text}\n"))?;
            println!(">> {text}");
            if let Some(p) = paster {
                if !text.is_empty() {
                    if let Err(e) = p.paste(&text).await {
                        eprintln!("paste failed: {e}");
                    }
                }
            }
            if let Some(cmd) = exec {
                use tokio::io::AsyncWriteExt;
                let mut child = tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(cmd)
                    .env("INDEX01_TEXT", &text)
                    .env("INDEX01_WAV", &wav)
                    .env("INDEX01_TXT", &txt)
                    .stdin(std::process::Stdio::piped())
                    .spawn()?;
                if let Some(mut stdin) = child.stdin.take() {
                    stdin.write_all(text.as_bytes()).await?;
                }
                let st = child.wait().await?;
                if !st.success() {
                    eprintln!("--exec exited with {st}");
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
