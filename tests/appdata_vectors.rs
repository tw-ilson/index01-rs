//! Application-data vectors generated from the real libppcommon (tests/gen_appdata_vectors.py).
use index01::appdata::{build_app_data, user_fingerprint};

#[derive(serde::Deserialize)]
struct V {
    user_id: String,
    unix_time: u32,
    fingerprint: u32,
    blob: String,
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn appdata_matches_library() {
    let vs: Vec<V> = serde_json::from_str(include_str!("appdata_vectors.json")).unwrap();
    assert!(vs.len() >= 60);
    for v in vs {
        assert_eq!(
            user_fingerprint(&v.user_id),
            v.fingerprint,
            "{:?}",
            v.user_id
        );
        assert_eq!(
            build_app_data(&v.user_id, v.unix_time),
            unhex(&v.blob),
            "{:?}",
            v.user_id
        );
    }
}
