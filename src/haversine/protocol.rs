//! Index 01 / Telesto protocol codec, derived from the decompiled
//! `libhaversinesatellitelibrary.so` (see `docs/PROTOCOL.md` for citations).

use std::fmt;

use uuid::{uuid, Uuid};

// --- UUIDs (java_src/.../transport/HaversineUUID.java) ---
pub const HAVERSINE_SERVICE_128: Uuid = uuid!("607b5c9b-3700-4e94-f44a-2df900bcb0c3");
pub const HAVERSINE_SERVICE_16: Uuid = uuid!("0000fcc9-0000-1000-8000-00805f9b34fb");
pub const TELESTO_CTRL_UUID: Uuid = uuid!("c0ef558a-2058-fabf-a140-8d5acde50b39");
pub const TELESTO_DATA_UUID: Uuid = uuid!("daad3d52-237c-90a7-b54b-8854a134d801");
pub const SYSTEM_INPUT_UUID: Uuid = uuid!("1d1f4039-23f5-33b2-c24e-704351f20585");
pub const CCC_DESCRIPTOR_UUID: Uuid = uuid!("00002902-0000-1000-8000-00805f9b34fb");

/// Company id used in the scan filter (CentralManager.buildScanFilters); 0x0EEA, confirmed on hardware.
pub const HAVERSINE_COMPANY_ID: u16 = 3818;

// --- Telesto virtual addresses (TelestoVirtualAddress.java + constant tables) ---
pub const APPLICATION_DATA_STORE: u32 = 0x4000_0000;
pub const PROGRAMMED_SERIAL_ADDR: u32 = 0x4000_0004;
pub const CORE_DUMP_BASE: u32 = 0x4001_0000;
pub const CONFIG_BASE: u32 = 0x4003_0000;
pub const UNIX_TIME_ADDR: u32 = 0x4003_0001;
pub const COLLECTION_BASE: u32 = 0x4002_0000;
pub const STORED_COLLECTION_INDEXES: u32 = 0x4003_0005;
pub const PLATFORM_VERSIONS: u32 = 0x4003_0006;
pub const SERIAL_NUMBER_ADDR: u32 = 0x4003_0008;
pub const ADVERTISING_DATA_ADDR: u32 = 0x4003_000E;
pub const RSSI_ADDR: u32 = 0x4003_000F;
pub const SENSOR_CONFIG_BASE: u32 = 0x4005_0000;
pub const FIFO_BASE: u32 = 0x4004_0000;
pub const PRIMARY_IMAGE_BASE: u32 = 0x4006_0000;
pub const REBOOT_TRACKING_ADDR: u32 = 0x4006_0002;

/// Native per-collection sink cap (FUN_001104b8).
pub const COLLECTION_MAX: usize = 0xA0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TelestoOp {
    NoOp = 0,
    EraseMemory = 1,
    ProgramMemory = 2,
    ReadMemory = 3,
    CancelOperation = 4,
}

/// Operation-error domain (HaversineError.java, HaversineError_fromOperationCode).
pub mod status {
    pub const NONE: u32 = 0;
    pub const UNKNOWN: u32 = 1;
    pub const CONFIG_ERROR: u32 = 2;
    pub const OVERFLOW_ERROR: u32 = 3;
    pub const MISSING_DATA: u32 = 4;
    pub const END_ENCOUNTERED: u32 = 5;
    pub const LOG_DATA_ALREADY_SAVED: u32 = 6;
    pub const UNEXPECTED_VERSION: u32 = 7;
    pub const UNEXPECTED_DATA_SIZE: u32 = 8;
    pub const CANCELLED_BY_REQUEST: u32 = 0x41;
    pub const CANCELLED_BY_WRITE: u32 = 0x42;
    pub const BAD_REQUEST: u32 = 0x43;
    pub const TRANSPORT_CLOSED: u32 = 0x44;

    pub fn name(code: u32) -> Option<&'static str> {
        Some(match code {
            NONE => "NONE",
            UNKNOWN => "UNKNOWN",
            CONFIG_ERROR => "CONFIG_ERROR",
            OVERFLOW_ERROR => "OVERFLOW_ERROR",
            MISSING_DATA => "MISSING_DATA",
            END_ENCOUNTERED => "END_ENCOUNTERED",
            LOG_DATA_ALREADY_SAVED => "LOG_DATA_ALREADY_SAVED",
            UNEXPECTED_VERSION => "UNEXPECTED_VERSION",
            UNEXPECTED_DATA_SIZE => "UNEXPECTED_DATA_SIZE",
            CANCELLED_BY_REQUEST => "CANCELLED_BY_REQUEST",
            CANCELLED_BY_WRITE => "CANCELLED_BY_WRITE",
            BAD_REQUEST => "BAD_REQUEST",
            TRANSPORT_CLOSED => "TRANSPORT_CLOSED",
            _ => return None,
        })
    }
}

/// Non-zero status in a ctrl response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelestoError(pub u32);

impl fmt::Display for TelestoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match status::name(self.0) {
            Some(n) => write!(f, "telesto status {} ({n})", self.0),
            None => write!(f, "telesto status {} (UNKNOWN_0x{:02X})", self.0, self.0),
        }
    }
}

impl std::error::Error for TelestoError {}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("manufacturer data must be 6 or 8 bytes, got {0}")]
    AdvertisementLength(usize),
    #[error("ctrl response must be 12 bytes, got {0}")]
    CtrlResponseLength(usize),
    #[error("stored indexes payload must be 4 bytes, got {0}")]
    IndexesLength(usize),
}

// --- Advertisement (HaversineAdvertisementData_parseManufacturedData @0x10ea28) ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdvertisementData {
    pub cacheable_state_fingerprint: u32,
    pub collection_count: u8,
    pub needs_servicing: bool,
    pub in_collection_state: bool,
    pub is_moving: bool,
    pub has_debug_info: bool,
    /// Flags bit 3: parsed by the library (result bit 32) but dropped by the JNI constructor.
    pub flag_bit3: bool,
}

impl AdvertisementData {
    pub fn is_failsafe(&self) -> bool {
        self.cacheable_state_fingerprint == 0xDEAD_DEAD
    }

    pub fn is_ptf_image(&self) -> bool {
        self.cacheable_state_fingerprint == 0xDEAD_BEEF
    }
}

/// Parse the 6- or 8-byte manufacturer-specific data field.
/// The 8-byte form starts with the 2-byte company id, which is skipped.
/// DEVIATION: other lengths are an error; the library returns an all-zero result.
pub fn parse_advertisement(raw: &[u8]) -> Result<AdvertisementData, ParseError> {
    let raw = match raw.len() {
        8 => &raw[2..],
        6 => raw,
        n => return Err(ParseError::AdvertisementLength(n)),
    };
    let flags = raw[5];
    // Bit mapping derived from the JNI constructor call @0x10c2f4.
    Ok(AdvertisementData {
        cacheable_state_fingerprint: u32::from_le_bytes(raw[0..4].try_into().unwrap()),
        collection_count: raw[4],
        needs_servicing: (flags >> 6) & 1 != 0,
        in_collection_state: (flags >> 5) & 1 != 0,
        is_moving: (flags >> 7) & 1 != 0,
        has_debug_info: (flags >> 4) & 1 != 0,
        flag_bit3: (flags >> 3) & 1 != 0,
    })
}

// --- Telesto request / ctrl response ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelestoRequest {
    pub op: TelestoOp,
    pub address: u32,
    pub offset: u32,
    pub length: u32,
}

impl TelestoRequest {
    pub fn new(op: TelestoOp, address: u32, offset: u32, length: u32) -> Self {
        Self {
            op,
            address,
            offset,
            length,
        }
    }

    pub fn read(address: u32, length: u32) -> Self {
        Self::new(TelestoOp::ReadMemory, address, 0, length)
    }

    /// 13-byte little-endian header written to the ctrl characteristic.
    pub fn encode(&self) -> [u8; 13] {
        let mut b = [0u8; 13];
        b[0] = self.op as u8;
        b[1..5].copy_from_slice(&self.address.to_le_bytes());
        b[5..9].copy_from_slice(&self.offset.to_le_bytes());
        b[9..13].copy_from_slice(&self.length.to_le_bytes());
        b
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtrlResponse {
    pub status: u32,
    /// Bytes 8..12: how many data bytes the operation delivers (READ completes once it has them).
    pub data_length: u32,
    pub raw: Vec<u8>,
}

impl CtrlResponse {
    pub fn check(&self) -> Result<(), TelestoError> {
        if self.status == 0 {
            Ok(())
        } else {
            Err(TelestoError(self.status))
        }
    }
}

/// Decode the 12-byte ctrl response `{u32 status, u32, u32 data_length}` (TelestoController
/// buffers it at +0x78; status is read at +0x78, data_length at +0x80, the middle word is unused).
pub fn decode_ctrl_response(raw: &[u8]) -> Result<CtrlResponse, ParseError> {
    if raw.len() != 12 {
        return Err(ParseError::CtrlResponseLength(raw.len()));
    }
    let w = |i: usize| u32::from_le_bytes(raw[i..i + 4].try_into().unwrap());
    Ok(CtrlResponse {
        status: w(0),
        data_length: w(8),
        raw: raw.to_vec(),
    })
}

// --- Stored collection indexes (0x40030005, 4 bytes) ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredCollectionIndexes {
    pub range_start: u16,
    pub range_end: u16,
}

impl StoredCollectionIndexes {
    /// The native loop transfers while index != rangeEnd, so rangeEnd is exclusive.
    pub fn iter(&self) -> std::ops::Range<u16> {
        self.range_start..self.range_end
    }
}

pub fn parse_stored_collection_indexes(raw: &[u8]) -> Result<StoredCollectionIndexes, ParseError> {
    if raw.len() < 4 {
        return Err(ParseError::IndexesLength(raw.len()));
    }
    Ok(StoredCollectionIndexes {
        range_start: u16::from_le_bytes([raw[0], raw[1]]),
        range_end: u16::from_le_bytes([raw[2], raw[3]]),
    })
}

/// Address of collection N: `COLLECTION_BASE | (N & 0xffff)`.
pub fn collection_address(index: u32) -> u32 {
    COLLECTION_BASE | (index & 0xFFFF)
}

/// 2-byte u16 LE end index (Java parseTransferOperationResult @0x10b8b8).
pub fn parse_transfer_operation_result(raw: &[u8]) -> Option<u16> {
    match raw {
        [a, b] => Some(u16::from_le_bytes([*a, *b])),
        _ => None,
    }
}

/// firstCollectionToTransferInRange semantics (HaversineTransferDelegate).
pub fn resume_index(range_start: u16, range_end: u16, last_successful: Option<u16>) -> u16 {
    let Some(last) = last_successful else {
        return range_start;
    };
    let next = last.wrapping_add(1);
    // Everything up to rangeEnd already transferred → no work.
    if range_end.wrapping_sub(next) >= 0x8000 {
        return range_end;
    }
    range_start.max(next)
}

/// TelestoLengthPrefixedData_create @0x111f1c: `[u32 LE totalLen][payload]`.
pub fn length_prefixed(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 4);
    out.extend_from_slice(&((data.len() as u32) + 4).to_le_bytes());
    out.extend_from_slice(data);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn request_read_platform_versions_matches_rodata_table() {
        // bytes at 0x106b28 in libhaversinesatellitelibrary.so
        assert_eq!(
            TelestoRequest::read(0x4003_0006, 4).encode().to_vec(),
            hex("03060003400000000004000000")
        );
    }

    #[test]
    fn request_read_stored_indexes_matches_rodata_table() {
        // bytes at 0x106c30
        assert_eq!(
            TelestoRequest::read(0x4003_0005, 4).encode().to_vec(),
            hex("03050003400000000004000000")
        );
    }

    #[test]
    fn request_read_collection_base_layout() {
        let raw = TelestoRequest::read(collection_address(0x123), 0).encode();
        assert_eq!(raw[0], 3);
        assert_eq!(
            u32::from_le_bytes(raw[1..5].try_into().unwrap()),
            0x4002_0123
        );
        assert_eq!(&raw[5..13], &[0u8; 8]);
    }

    #[test]
    fn erase_collections_encoding_from_decompile() {
        // HaversineEraseCollectionsOperation_init @0x111484
        let r = TelestoRequest::new(TelestoOp::EraseMemory, 0x4003_0000, 0, 0x200);
        assert_eq!(r.encode().to_vec(), hex("01000003400000000000020000"));
    }

    #[test]
    fn decode_ctrl_response_status_little_endian() {
        let mut raw = 0u32.to_le_bytes().to_vec();
        raw.extend([0xAA; 8]);
        let resp = decode_ctrl_response(&raw).unwrap();
        assert_eq!(resp.status, 0);
        assert_eq!(resp.raw.len(), 12);

        let mut raw = 0x43u32.to_le_bytes().to_vec();
        raw.extend([0; 8]);
        let resp = decode_ctrl_response(&raw).unwrap();
        assert_eq!(resp.status, status::BAD_REQUEST);
        assert_eq!(resp.check(), Err(TelestoError(0x43)));
        assert_eq!(
            TelestoError(0x43).to_string(),
            "telesto status 67 (BAD_REQUEST)"
        );
    }

    #[test]
    fn decode_ctrl_response_bad_lengths_rejected() {
        assert!(decode_ctrl_response(&[0, 0]).is_err());
        assert!(decode_ctrl_response(&[0; 4]).is_err());
        assert!(decode_ctrl_response(&[0; 13]).is_err());
    }

    #[test]
    fn status_values_match_haversine_error_java() {
        assert_eq!(status::NONE, 0);
        assert_eq!(status::UNEXPECTED_VERSION, 7);
        assert_eq!(status::CANCELLED_BY_REQUEST, 0x41);
        assert_eq!(status::TRANSPORT_CLOSED, 0x44);
    }

    #[test]
    fn advertisement_6byte_payload() {
        let mut raw = hex("34125689");
        raw.extend([7, 0b1011_0000]);
        let adv = parse_advertisement(&raw).unwrap();
        assert_eq!(adv.cacheable_state_fingerprint, 0x8956_1234);
        assert_eq!(adv.collection_count, 7);
        assert!(adv.is_moving);
        assert!(!adv.needs_servicing);
        assert!(adv.in_collection_state);
        assert!(adv.has_debug_info);
    }

    #[test]
    fn advertisement_8byte_skips_company_id() {
        let mut raw = HAVERSINE_COMPANY_ID.to_le_bytes().to_vec();
        raw.extend(hex("34125689"));
        raw.extend([1, 0b0001_0000]);
        let adv = parse_advertisement(&raw).unwrap();
        assert_eq!(adv.cacheable_state_fingerprint, 0x8956_1234);
        assert_eq!(adv.collection_count, 1);
        assert!(adv.has_debug_info);
    }

    #[test]
    fn advertisement_real_ring_after_user_programmed() {
        // Captured from AA:BB:CC:DD:EE:FF: mfr {0x0eea: 9f71ee790000}
        let adv = parse_advertisement(&hex("9f71ee790000")).unwrap();
        assert_eq!(adv.cacheable_state_fingerprint, 0x79EE_719F);
        assert_eq!(adv.collection_count, 0);
        assert!(!adv.needs_servicing);
    }

    #[test]
    fn advertisement_special_fingerprints() {
        let mut raw = 0xDEAD_DEADu32.to_le_bytes().to_vec();
        raw.extend([0, 0]);
        assert!(parse_advertisement(&raw).unwrap().is_failsafe());
        let mut raw = 0xDEAD_BEEFu32.to_le_bytes().to_vec();
        raw.extend([0, 0]);
        assert!(parse_advertisement(&raw).unwrap().is_ptf_image());
    }

    #[test]
    fn advertisement_rejects_bad_lengths() {
        assert!(parse_advertisement(&[1; 5]).is_err());
        assert!(parse_advertisement(&[1; 7]).is_err());
    }

    #[test]
    fn stored_collection_indexes() {
        let mut raw = 0x0100u16.to_le_bytes().to_vec();
        raw.extend(0x0105u16.to_le_bytes());
        let idx = parse_stored_collection_indexes(&raw).unwrap();
        assert_eq!((idx.range_start, idx.range_end), (0x0100, 0x0105));
        assert_eq!(
            idx.iter().collect::<Vec<_>>(),
            vec![0x100, 0x101, 0x102, 0x103, 0x104]
        );
        assert!(parse_stored_collection_indexes(&[0; 3]).is_err());
    }

    #[test]
    fn transfer_operation_result() {
        assert_eq!(
            parse_transfer_operation_result(&0x0143u16.to_le_bytes()),
            Some(0x0143)
        );
        assert_eq!(parse_transfer_operation_result(&[1]), None);
        assert_eq!(parse_transfer_operation_result(&[1, 0, 0]), None);
    }

    #[test]
    fn resume_index_semantics() {
        assert_eq!(resume_index(10, 20, None), 10);
        assert_eq!(resume_index(10, 20, Some(4)), 10);
        assert_eq!(resume_index(10, 20, Some(11)), 12);
        assert_eq!(resume_index(10, 20, Some(19)), 20);
        assert_eq!(resume_index(10, 20, Some(20)), 20);
    }

    #[test]
    fn length_prefixed_helper() {
        assert_eq!(
            length_prefixed(b"ABCD"),
            [&8u32.to_le_bytes()[..], b"ABCD"].concat()
        );
    }
}
