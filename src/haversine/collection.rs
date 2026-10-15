//! Decoder for Index 01 "collection" blobs.
//!
//! A faithful port of `libppcommon` (Ghidra decompile: `decomp/ppcommon_all.c`, arm64
//! addresses cited). `tests/vectors.rs` checks it against outputs of the real library.
//! Places where this port deliberately differs are marked `DEVIATION:`.
//!
//! Blob layout (GSParseRecordsInRawData @0x123e3c): a header, then TLV records until the end.
//! Three header variants, tried in this order:
//! - (b) `blob[3] != 0`: `blob[0..3]` = 24-bit BIG-endian `len - 3`, records at 3
//! - (a) otherwise: `blob[0..3]` = 24-bit LE `len`, `blob[3] == 0`, records at 4
//! - (c) fallback when `blob[0] == 0xFF`: `blob[1..3]` = u16 LE `len - 3`, records at 3
//!
//! A header mismatch gives error 6; `len < 3` gives error 7.
//!
//! Records: u8 type, then a u32 LE length (types 0x50/0x51) or a u16 LE length (others),
//! then the payload. Type 0x23 terminates the parse with error 1 (records so far are kept).
//! Unknown types give error 6.
//!
//! Getters (PPCollection_* @0x125254..0x1259e0):
//! - 0x01 serial: payload bytes as uppercase hex
//! - 0x02 unix time: see [`parse_unix_time`]
//! - 0x37 battery: u16 LE millivolts
//! - 0x54 lifetime count: u32 LE
//! - 0x25 platform versions: u32 LE (hwMajor, hwMinor, fwMajor, fwMinor)
//! - 0x27 application data: raw payload
//! - 0x53 button sequence: u32 LE bitfield (bit i set = long press) + u32 LE count
//! - 0x52 multipart: u32 LE start index, u8 is_multipart, u8 is_final
//! - 0x50 raw audio: u32 LE sample rate, then u16 LE samples (odd trailing byte dropped)
//! - 0x51 Rice audio: u8 param, u32 LE total bit count, u32 LE sample rate, bitstream

use std::collections::BTreeMap;
use std::fmt;

/// `raw_data_codes_e` names, dumped from the x86_64 .so.
pub fn record_type_name(t: u8) -> Option<&'static str> {
    Some(match t {
        0xFF => "RECORDS_DATA_TRANSFER",
        0x01 => "DEVICE_ID",
        0x02 => "SWING_UTC",
        0x03 => "IMPACT_TIME",
        0x04 => "ACCEL_CALIBRATION",
        0x05 => "GYRO_CALIBRATION",
        0x06 => "IMU_DATA",
        0x07 => "MAG_CALIBRATION",
        0x08 => "MAG_DATA",
        0x09 => "SENSOR_TEMPERATURE",
        0x0A => "TARGET_LINE_AIM",
        0x0B => "SWING_SETUP",
        0x0C => "CLUB_SETTINGS",
        0x0D => "VSR_DATA",
        0x0E => "MULTI_ACCEL_DATA",
        0x10 => "ACCEL_2_CALIBRATION",
        0x11 => "GYRO_2_CALIBRATION",
        0x12 => "HACCEL_1_CALIBRATION",
        0x13 => "HACCEL_2_CALIBRATION",
        0x21 => "ST_FIFO_COMPRESSED",
        0x22 => "ST_SENSOR_CONFIG",
        0x23 => "FULL_CLUB_SETTINGS",
        0x24 => "ALL_SENSOR_CALIBRATIONS",
        0x25 => "PLATFORM_VERSIONS",
        0x26 => "USER_DATA",
        0x27 => "APPLICATION_DATA_STORE",
        0x28 => "ST_FIFO_FIRMWARE_COMPRESSED",
        0x29 => "DETECTOR_DATA",
        0x30 => "LAST_STATIONARY_DATA",
        0x31 => "STATIONARY_DATA_VERSION",
        0x32 => "CROPPED_STATIONARY_DATA",
        0x33 => "STATIONARY_DATA_SENSOR_CONFIGS",
        0x34 => "COLLECTION_SENSOR_CONFIGS",
        0x35 => "SWING_TIME_CORRECTION",
        0x37 => "BATTERY_VOLTAGE",
        0x50 => "UNCOMPRESSED_16BIT_AUDIO",
        0x51 => "COMPRESSED_16BIT_AUDIO",
        0x52 => "COLLECTION_MULTI_PART_INFO",
        0x53 => "BUTTON_PRESS_SEQUENCE",
        0x54 => "LIFETIME_COLLECTION_COUNT",
        _ => return None,
    })
}

/// Record types the library's switch accepts (oracle-enumerated over all u8 values).
fn is_known_type(t: u8) -> bool {
    matches!(t, 0x00..=0x12 | 0x21..=0x29 | 0x30..=0x35 | 0x37 | 0x50..=0x54)
}

/// Terminator record type (FULL_CLUB_SETTINGS).
const TERMINATOR: u8 = 0x23;

/// The unix time getter fails unless the result exceeds this.
const UNIX_TIME_MIN: u32 = 0x4E41_FCA7;

/// `PPErr` codes, from the Java enum order and createFromBinaryData behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PPErr(pub u32);

impl PPErr {
    pub const SUCCESS: PPErr = PPErr(0);
    /// A FULL_CLUB_SETTINGS record terminated the parse.
    pub const FAIL: PPErr = PPErr(1);
    pub const PARAMETER_MISSING: PPErr = PPErr(2);
    pub const INVALID_PARAMETER: PPErr = PPErr(3);
    /// Length header mismatch or unknown record type.
    pub const INVALID_RAW_DATA: PPErr = PPErr(6);
    /// Blob shorter than 3 bytes.
    pub const RAW_DATA_TOO_SHORT: PPErr = PPErr(7);
    pub const MALLOC_FAILED: PPErr = PPErr(0x13);

    pub fn name(self) -> &'static str {
        match self.0 {
            0 => "PPSuccess",
            1 => "PPFail",
            2 => "PPParameterMissing",
            3 => "PPInvalidParameter",
            6 => "PPInvalidRawData",
            7 => "PPRawDataTooShort",
            0x13 => "PPMallocFailed",
            _ => "?",
        }
    }
}

impl fmt::Display for PPErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "parse error {} ({})", self.0, self.name())
    }
}

impl std::error::Error for PPErr {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonPress {
    Short,
    Long,
}

impl ButtonPress {
    pub fn as_str(self) -> &'static str {
        match self {
            ButtonPress::Short => "short",
            ButtonPress::Long => "long",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Audio {
    pub sample_rate_hz: u32,
    /// Raw u16 bit patterns as the library returns them; the app treats them as i16.
    pub samples: Vec<u16>,
    pub is_multipart: bool,
    pub is_final: bool,
    pub collection_start_index: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Collection {
    /// Record type → payload. A repeated type keeps the last occurrence, like the library.
    pub records: BTreeMap<u8, Vec<u8>>,
    pub serial: String,
    pub unix_time: u32,
    pub battery_mv: u16,
    pub lifetime_count: u32,
    pub button_sequence: Vec<ButtonPress>,
    pub app_data: Vec<u8>,
    pub platform_versions: u32,
    pub audio: Audio,
}

/// `data[a..b]` with Python slice clamping.
fn sl(data: &[u8], a: usize, b: usize) -> &[u8] {
    let a = a.min(data.len());
    &data[a..b.clamp(a, data.len())]
}

/// Little-endian unsigned integer from up to 8 bytes (shorter slices read as fewer bytes).
fn le(b: &[u8]) -> u64 {
    b.iter().rev().fold(0u64, |acc, &x| (acc << 8) | x as u64)
}

fn be(b: &[u8]) -> u64 {
    b.iter().fold(0u64, |acc, &x| (acc << 8) | x as u64)
}

type Records = BTreeMap<u8, Vec<u8>>;

/// GSParseRecordsInRawData @0x123e3c. Returns the records parsed so far and the error code.
fn parse_tlv(data: &[u8]) -> (Records, PPErr) {
    let n = data.len();
    if n < 3 {
        return (Records::new(), PPErr::RAW_DATA_TOO_SHORT);
    }
    let declared_be = be(&data[0..3]) as usize;
    if n == 3 {
        let e = if declared_be == 0 {
            PPErr::SUCCESS
        } else {
            PPErr::INVALID_RAW_DATA
        };
        return (Records::new(), e);
    }
    let off = if data[3] != 0 {
        (declared_be == n - 3).then_some(3)
    } else {
        (le(&data[0..3]) as usize == n).then_some(4)
    };
    match off {
        Some(off) => parse_records(data, off),
        None if data[0] == 0xFF => {
            if le(&data[1..3]) as usize != n - 3 {
                return (Records::new(), PPErr::INVALID_RAW_DATA);
            }
            parse_records(data, 3)
        }
        None => (Records::new(), PPErr::INVALID_RAW_DATA),
    }
}

fn parse_records(data: &[u8], mut off: usize) -> (Records, PPErr) {
    let mut records = Records::new();
    while off < data.len() {
        let t = data[off];
        if t == TERMINATOR {
            return (records, PPErr::FAIL);
        }
        if !is_known_type(t) {
            return (records, PPErr::INVALID_RAW_DATA);
        }
        let hdr = if t == 0x50 || t == 0x51 { 5 } else { 3 };
        let len = le(sl(data, off + 1, off + hdr)) as usize;
        records.insert(t, sl(data, off + hdr, off + hdr + len).to_vec());
        off = off.saturating_add(hdr).saturating_add(len);
    }
    (records, PPErr::SUCCESS)
}

/// GSGetHardwareSerialNumber: each byte as `%02X` (oracle: `c0 ff ee 12 34 56` → `C0FFEE123456`).
fn parse_serial(payload: &[u8]) -> String {
    payload.iter().map(|b| format!("{b:02X}")).collect()
}

/// PPCollection_unixTime @0x1255a0.
///
/// A 4-byte payload is u32 LE seconds. A longer payload is 48-bit BIG-endian milliseconds,
/// divided down to seconds, unless bytes 4 and 5 are both zero (then it is u32 LE seconds).
/// A 0x35 SWING_TIME_CORRECTION record adds its i32 LE value plus 1 (wrapping u32).
/// Returns `None` when the result is not above 0x4E41FCA7 (the getter fails with status 7).
fn parse_unix_time(payload: &[u8], time_offset: Option<&[u8]>) -> Option<u32> {
    if payload.len() < 4 {
        return None;
    }
    let u32_le = le(&payload[0..4]) as u32;
    let low_zero =
        payload.get(4).copied().unwrap_or(0) == 0 && payload.get(5).copied().unwrap_or(0) == 0;
    let mut secs = if payload.len() == 4 || low_zero {
        u32_le
    } else {
        (be(sl(payload, 0, 6)) / 1000) as u32
    };
    if let Some(off) = time_offset.filter(|o| o.len() >= 4) {
        let corr = i32::from_le_bytes(off[0..4].try_into().unwrap());
        secs = secs.wrapping_add(corr as u32).wrapping_add(1);
    }
    (secs > UNIX_TIME_MIN).then_some(secs)
}

/// DEVIATION: the library has no cap. The bitfield is 32 bits, so presses past that are
/// always short; capping stops a corrupt count from allocating billions of entries.
const MAX_BUTTON_PRESSES: u32 = 1024;

/// PPCollection_buttonPressSequenceString @0x1258dc.
fn parse_button_sequence(payload: &[u8]) -> Vec<ButtonPress> {
    if payload.len() < 8 {
        return Vec::new();
    }
    let bits = le(&payload[0..4]) as u32;
    let count = (le(&payload[4..8]) as u32).min(MAX_BUTTON_PRESSES);
    (0..count)
        .map(|i| {
            if i < 32 && (bits >> i) & 1 == 1 {
                ButtonPress::Long
            } else {
                ButtonPress::Short
            }
        })
        .collect()
}

/// Rice decoder error: the stream ran out of bits (normal end) or the parameter is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiceEnd {
    /// Parameter byte > 0xEF (DDRiceCompressionChannel_init guard), library code 1.
    BadParam,
    /// Bit budget exhausted, library code 3 — the normal way a stream ends.
    Exhausted,
}

/// Decode a Rice-compressed sample stream (DDRiceDecompression* @0x125f64..0x1262f4).
///
/// - Bits are read MSB-first, bytes in order, with a hard budget of `total_bits`
///   (the record's totalBitCount; defaults to `data.len() * 8`).
/// - Parameter byte P: `k = P & 0xF` (shift), `t = P >> 4` (unary threshold).
/// - Per diff (decodeDiff @0x126098): a 1 bit means diff 0. Otherwise count zeros from z = 1
///   while z < t; on a 1, read a sign bit: diff = z, or `(0x10000 >> k) - z` if negative.
///   If z reaches t, escape: read `16 - k` raw bits as the diff.
/// - nextWord @0x1262f4: double integrator then `<< k`, all wrapping u16.
///
/// Returns the samples decoded before the stream ended and why it ended.
///
/// DEVIATION: a budget larger than `data` ends the stream at the end of `data`; the library
/// would read past its buffer.
pub fn rice_decode(data: &[u8], param: u8, total_bits: Option<u64>) -> (Vec<u16>, RiceEnd) {
    if param > 0xEF {
        return (Vec::new(), RiceEnd::BadParam);
    }
    let k = (param & 0xF) as u32;
    let threshold = (param >> 4) as u32;
    let budget = total_bits.unwrap_or(data.len() as u64 * 8);
    let mut pos: u64 = 0;
    let mut read_bit = || -> Option<u32> {
        if pos >= budget {
            return None;
        }
        let byte = *data.get((pos >> 3) as usize)?;
        let b = (byte >> (7 - (pos & 7))) & 1;
        pos += 1;
        Some(b as u32)
    };

    let mut samples = Vec::new();
    let (mut two, mut one) = (0u16, 0u16);
    loop {
        let Some(diff) = (|| -> Option<u16> {
            if read_bit()? == 1 {
                return Some(0);
            }
            let mut z = 1u32;
            while z < threshold {
                if read_bit()? == 1 {
                    let negative = read_bit()? == 1;
                    let d = if negative {
                        (0x10000u32 >> k).wrapping_sub(z)
                    } else {
                        z
                    };
                    return Some(d as u16);
                }
                z += 1;
            }
            let mut w = 0u32;
            for _ in 0..(16 - k) {
                w = (w << 1) | read_bit()?;
            }
            Some(w as u16)
        })() else {
            return (samples, RiceEnd::Exhausted);
        };
        two = two.wrapping_add(diff);
        one = one.wrapping_add(two);
        samples.push(((one as u32) << k) as u16);
    }
}

/// Parse a full collection blob. A terminator record (PPFail) still returns the records
/// parsed before it; any other parse error is returned as `Err`.
pub fn parse_collection(data: &[u8]) -> Result<Collection, PPErr> {
    let (records, err) = parse_tlv(data);
    if err != PPErr::SUCCESS && err != PPErr::FAIL {
        return Err(err);
    }
    let mut col = Collection::default();
    let r = |t: u8| records.get(&t).map(Vec::as_slice);

    if let Some(p) = r(0x01) {
        col.serial = parse_serial(p);
    }
    if let Some(p) = r(0x02) {
        col.unix_time = parse_unix_time(p, r(0x35)).unwrap_or(0);
    }
    if let Some(p) = r(0x37).filter(|p| p.len() >= 2) {
        col.battery_mv = le(&p[0..2]) as u16;
    }
    if let Some(p) = r(0x54).filter(|p| p.len() >= 4) {
        col.lifetime_count = le(&p[0..4]) as u32;
    }
    if let Some(p) = r(0x53) {
        col.button_sequence = parse_button_sequence(p);
    }
    if let Some(p) = r(0x27) {
        col.app_data = p.to_vec();
    }
    if let Some(p) = r(0x25).filter(|p| p.len() >= 4) {
        col.platform_versions = le(&p[0..4]) as u32;
    }

    let audio = &mut col.audio;
    if let Some(p) = r(0x52).filter(|p| p.len() >= 6) {
        audio.collection_start_index = le(&p[0..4]) as u32;
        audio.is_multipart = p[4] != 0;
        audio.is_final = p[5] != 0;
    }
    if let Some(p) = r(0x50) {
        // GSUncompressedAudioData: u32 LE rate, then u16 LE samples; odd trailing byte dropped.
        audio.sample_rate_hz = le(sl(p, 0, 4)) as u32;
        audio.samples = sl(p, 4, p.len())
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
    } else if let Some(p) = r(0x51).filter(|p| p.len() >= 9) {
        // GSCompressedAudioData: param @0, u32 totalBitCount @1, u32 sampleRate @5, bitstream @9.
        let param = p[0];
        let total_bits = le(&p[1..5]);
        audio.sample_rate_hz = le(&p[5..9]) as u32;
        // Note: param 0x00 decodes normally (k = 0, every non-zero diff escapes to 16 raw bits);
        // the Python port's "param 0 yields no samples" was wrong — see tests/vectors.json.
        let (samples, end) = rice_decode(&p[9..], param, Some(total_bits));
        if end == RiceEnd::BadParam {
            return Err(PPErr::INVALID_PARAMETER);
        }
        audio.samples = samples;
    }
    col.records = records;
    Ok(col)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(t: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![t];
        if t == 0x50 || t == 0x51 {
            v.extend((payload.len() as u32).to_le_bytes());
        } else {
            v.extend((payload.len() as u16).to_le_bytes());
        }
        v.extend_from_slice(payload);
        v
    }

    fn blob_le(records: &[Vec<u8>]) -> Vec<u8> {
        let body = records.concat();
        let n = 4 + body.len();
        let mut v = vec![n as u8, (n >> 8) as u8, (n >> 16) as u8, 0];
        v.extend(body);
        v
    }

    #[test]
    fn short_blob_rejected() {
        assert_eq!(parse_collection(&[0, 0]), Err(PPErr::RAW_DATA_TOO_SHORT));
    }

    #[test]
    fn unknown_record_type_rejected() {
        assert_eq!(
            parse_collection(&blob_le(&[rec(0x40, b"ab")])),
            Err(PPErr::INVALID_RAW_DATA)
        );
    }

    #[test]
    fn terminator_keeps_earlier_records() {
        let c = parse_collection(&blob_le(&[
            rec(0x37, &3700u16.to_le_bytes()),
            rec(0x23, b"x"),
        ]))
        .unwrap();
        assert_eq!(c.battery_mv, 3700);
    }

    #[test]
    fn serial_hex_format() {
        let c = parse_collection(&blob_le(&[rec(
            0x01,
            &[0xc0, 0xff, 0xee, 0x12, 0x34, 0x56],
        )]))
        .unwrap();
        assert_eq!(c.serial, "C0FFEE123456");
    }

    #[test]
    fn unix_time_offset_plus_one() {
        let c = parse_collection(&blob_le(&[
            rec(0x02, &1_760_000_000u32.to_le_bytes()),
            rec(0x35, &5i32.to_le_bytes()),
        ]))
        .unwrap();
        assert_eq!(c.unix_time, 1_760_000_006);
    }

    #[test]
    fn unix_time_sanity_floor() {
        let c = parse_collection(&blob_le(&[rec(0x02, &0x4E41_FCA7u32.to_le_bytes())])).unwrap();
        assert_eq!(c.unix_time, 0);
    }

    #[test]
    fn button_sequence() {
        let mut p = 0b0110u32.to_le_bytes().to_vec();
        p.extend(4u32.to_le_bytes());
        let c = parse_collection(&blob_le(&[rec(0x53, &p)])).unwrap();
        let words: Vec<_> = c.button_sequence.iter().map(|b| b.as_str()).collect();
        assert_eq!(words, ["short", "long", "long", "short"]);
    }

    #[test]
    fn rice_bad_param() {
        assert_eq!(
            rice_decode(&[0xff], 0xF0, None),
            (vec![], RiceEnd::BadParam)
        );
    }

    #[test]
    fn default_collection() {
        let c = Collection::default();
        assert!(c.serial.is_empty() && c.unix_time == 0 && c.audio.samples.is_empty());
    }
}
