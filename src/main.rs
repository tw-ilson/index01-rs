//! Index 01 CLI. scan / info / list / pull / decode are read-only; setup, set-time and
//! erase write to the ring.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use index01::client::{self, Index01Client};
use index01::collection::parse_collection;
use index01::protocol as p;
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
) -> Result<(), Box<dyn std::error::Error>> {
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
) -> Result<(), Box<dyn std::error::Error>> {
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
    }
    Ok(())
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
