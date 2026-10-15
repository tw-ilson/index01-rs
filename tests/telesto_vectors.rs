//! Cross-check the Telesto controller port, request encoding, advertisement parser and status
//! table against the REAL libhaversinesatellitelibrary.so.
//! Regenerate with `tests/gen_telesto_vectors.py` (see its docstring).

use index01::haversine::controller::Transfer;
use index01::protocol::{parse_advertisement, status, TelestoOp, TelestoRequest};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct Scenario {
    note: String,
    op: u8,
    address: u32,
    offset: u32,
    length: u32,
    payload: Option<String>,
    events: Vec<(String, String)>,
    request: String,
    payload_sent: String,
    data: String,
    status: Option<u32>,
    errors: Vec<u32>,
}

#[derive(Deserialize)]
struct Advert {
    raw: String,
    needs_servicing: bool,
    in_collection_state: bool,
    collection_count: u8,
    is_moving: bool,
    has_debug_info: bool,
    flag_bit3: bool,
    cacheable_state_fingerprint: u32,
}

#[derive(Deserialize)]
struct Vectors {
    telesto: Vec<Scenario>,
    advertisement: Vec<Advert>,
    operation_errors: BTreeMap<String, Option<String>>,
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn vectors() -> Vectors {
    serde_json::from_str(include_str!("telesto_vectors.json")).unwrap()
}

fn op(code: u8) -> TelestoOp {
    match code {
        1 => TelestoOp::EraseMemory,
        2 => TelestoOp::ProgramMemory,
        3 => TelestoOp::ReadMemory,
        _ => unreachable!(),
    }
}

#[test]
fn telesto_controller_matches_real_library() {
    let v = vectors().telesto;
    assert_eq!(v.len(), 600);
    let (mut completed, mut errored, mut truncated) = (0, 0, 0);
    for s in &v {
        let req = TelestoRequest::new(op(s.op), s.address, s.offset, s.length);
        assert_eq!(
            req.encode().to_vec(),
            unhex(&s.request),
            "{} request",
            s.note
        );
        // The library sends the whole PROGRAM payload after the header; the runner sends the
        // same bytes in chunks.
        assert_eq!(
            s.payload.as_deref().map(unhex).unwrap_or_default(),
            unhex(&s.payload_sent),
            "{}",
            s.note
        );

        let mut t = Transfer::new(&req);
        let (mut data, mut errors, mut done, mut fed) = (Vec::new(), Vec::new(), None, 0usize);
        for (kind, hex) in &s.events {
            let bytes = unhex(hex);
            let r = if kind == "ctrl" {
                t.on_ctrl(&bytes)
            } else {
                fed += bytes.len();
                t.on_data(&bytes).map(|(kept, d)| {
                    data.extend_from_slice(kept);
                    d
                })
            };
            match r {
                Ok(Some(st)) => done = done.or(Some(st)),
                Ok(None) => {}
                Err(e) => errors.push(e as u32),
            }
        }
        assert_eq!(data, unhex(&s.data), "{} data", s.note);
        assert_eq!(done, s.status, "{} status", s.note);
        assert_eq!(errors, s.errors, "{} controller errors", s.note);
        completed += s.status.is_some() as usize;
        errored += !s.errors.is_empty() as usize;
        truncated += (s.op == 3 && data.len() < fed && s.errors.is_empty()) as usize;
    }
    assert!(
        completed > 300 && errored > 100 && truncated > 5,
        "{completed} {errored} {truncated}"
    );
}

#[test]
fn advertisement_matches_real_library() {
    for a in vectors().advertisement {
        let raw = unhex(&a.raw);
        match parse_advertisement(&raw) {
            Ok(p) => {
                assert_eq!(p.needs_servicing, a.needs_servicing, "{}", a.raw);
                assert_eq!(p.in_collection_state, a.in_collection_state, "{}", a.raw);
                assert_eq!(p.collection_count, a.collection_count, "{}", a.raw);
                assert_eq!(p.is_moving, a.is_moving, "{}", a.raw);
                assert_eq!(p.has_debug_info, a.has_debug_info, "{}", a.raw);
                assert_eq!(p.flag_bit3, a.flag_bit3, "{}", a.raw);
                assert_eq!(
                    p.cacheable_state_fingerprint, a.cacheable_state_fingerprint,
                    "{}",
                    a.raw
                );
            }
            // DEVIATION: Rust errors where the library returns all zeros.
            Err(_) => {
                assert!(raw.len() != 6 && raw.len() != 8);
                assert!(
                    !(a.needs_servicing
                        || a.in_collection_state
                        || a.is_moving
                        || a.has_debug_info
                        || a.flag_bit3)
                );
                assert_eq!((a.collection_count, a.cacheable_state_fingerprint), (0, 0));
            }
        }
    }
}

#[test]
fn status_codes_match_library_error_table() {
    let lib = vectors().operation_errors;
    // Every code the library describes (except the 0x40 domain base) has a name here.
    for (code, desc) in &lib {
        let code: u32 = code.parse().unwrap();
        if desc.is_some() && code != 0x40 {
            assert!(status::name(code).is_some(), "0x{code:x} {desc:?}");
        }
    }
    // 0x44 TRANSPORT_CLOSED comes from HaversineError.java; it is past the native table.
    assert_eq!(lib["68"], None);
    assert_eq!(status::name(0x44), Some("TRANSPORT_CLOSED"));
}
