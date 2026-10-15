//! Cross-check the collection decoder against outputs of the REAL libppcommon.so.
//! Regenerate with `tests/gen_vectors.py` (see its docstring).

use index01::collection::parse_collection;
use index01::wav::wav_bytes;
use serde::Deserialize;

#[derive(Deserialize)]
struct Fields {
    serial: String,
    unix_time: u32,
    battery_mv: u16,
    lifetime_count: u32,
    platform_versions: u32,
    app_data: String,
    button_sequence: Vec<String>,
}

#[derive(Deserialize)]
struct Audio {
    collection_start_index: u32,
    sample_rate_hz: u32,
    samples: Vec<u16>,
    is_multipart: bool,
    is_final: bool,
}

#[derive(Deserialize)]
struct Vector {
    note: String,
    blob: String,
    parse_err: u32,
    fields: Option<Fields>,
    audio: Option<Audio>,
    wav_hex: Option<String>,
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn collections_match_real_library() {
    let vectors: Vec<Vector> = serde_json::from_str(include_str!("vectors.json")).unwrap();
    assert!(vectors.len() > 250);
    let (mut audio_checked, mut wav_checked) = (0, 0);
    for v in &vectors {
        let blob = unhex(&v.blob);
        let got = parse_collection(&blob);
        let Some(f) = &v.fields else {
            assert_eq!(got.map_err(|e| e.0), Err(v.parse_err), "{}", v.note);
            continue;
        };
        let c = got.unwrap_or_else(|e| panic!("{}: {e}", v.note));
        assert_eq!(c.serial, f.serial, "{}", v.note);
        assert_eq!(c.unix_time, f.unix_time, "{}", v.note);
        assert_eq!(c.battery_mv, f.battery_mv, "{}", v.note);
        assert_eq!(c.lifetime_count, f.lifetime_count, "{}", v.note);
        assert_eq!(c.platform_versions, f.platform_versions, "{}", v.note);
        assert_eq!(c.app_data, unhex(&f.app_data), "{}", v.note);
        let words: Vec<&str> = c.button_sequence.iter().map(|b| b.as_str()).collect();
        assert_eq!(words, f.button_sequence, "{}", v.note);
        if let Some(a) = &v.audio {
            assert_eq!(c.audio.sample_rate_hz, a.sample_rate_hz, "{}", v.note);
            assert_eq!(c.audio.samples, a.samples, "{}", v.note);
            assert_eq!(
                c.audio.collection_start_index, a.collection_start_index,
                "{}",
                v.note
            );
            assert_eq!(c.audio.is_multipart, a.is_multipart, "{}", v.note);
            assert_eq!(c.audio.is_final, a.is_final, "{}", v.note);
            audio_checked += 1;
            if let Some(w) = &v.wav_hex {
                let rate = if a.sample_rate_hz == 0 {
                    16000
                } else {
                    a.sample_rate_hz
                };
                assert_eq!(
                    wav_bytes(&c.audio.samples, rate),
                    unhex(w),
                    "{} wav",
                    v.note
                );
                wav_checked += 1;
            }
        }
    }
    assert!(
        audio_checked > 100 && wav_checked > 100,
        "{audio_checked} {wav_checked}"
    );
}
