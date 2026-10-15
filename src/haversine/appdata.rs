//! Ring application data (the "user record" at `0x40000000`).
//!
//! Ports `PPRingUser_init`, `PPRingApplicationData_init`, `PPRingApplicationData_serialize`
//! (size 0x8d) and the fingerprint used by `PPRingApplicationData_fingerprintMatchesUserId`
//! (libppcommon). The library computes the fingerprint with a NEON-vectorised loop; the scalar
//! form below was recovered from its constants and matches the library on every test vector.
//!
//! Layout: `u32 LE 1 | u32 LE fingerprint | u32 LE unix_time | user_id (0x81 bytes, NUL-padded)`.

/// Serialized size (`PPRingApplicationData_serializedSize`).
pub const APP_DATA_SIZE: usize = 0x8d;
/// Max user id length; the field is 0x81 bytes including the terminating NUL.
pub const MAX_USER_ID_LEN: usize = 0x80;

/// Bob Jenkins' 32-bit integer hash (constants 0x7ed55d16, 0xc761c23c, … in the library).
fn jenkins32(mut a: u32) -> u32 {
    a = a.wrapping_add(0x7ed5_5d16).wrapping_add(a << 12);
    a = (a ^ 0xc761_c23c) ^ (a >> 19);
    a = a.wrapping_add(0x1656_67b1).wrapping_add(a << 5);
    a = a.wrapping_add(0xd3a2_646c) ^ (a << 9);
    a = a.wrapping_add(0xfd70_46c5).wrapping_add(a << 3);
    (a ^ 0xb55a_4f09) ^ (a >> 16)
}

/// Fingerprint of a user id: XOR of `jenkins32` over the 33 little-endian words of the id
/// zero-padded to 0x84 bytes. This is what the ring advertises as the cacheable-state fingerprint.
pub fn user_fingerprint(user_id: &str) -> u32 {
    let mut buf = [0u8; 0x84];
    let n = user_id.len().min(MAX_USER_ID_LEN);
    buf[..n].copy_from_slice(&user_id.as_bytes()[..n]);
    buf.chunks_exact(4)
        .map(|w| jenkins32(u32::from_le_bytes([w[0], w[1], w[2], w[3]])))
        .fold(0, |acc, h| acc ^ h)
}

/// Serialize the application data record for `user_id` at `unix_time`.
/// User ids longer than [`MAX_USER_ID_LEN`] bytes are truncated, like `PPRingUser_init`.
pub fn build_app_data(user_id: &str, unix_time: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(APP_DATA_SIZE);
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&user_fingerprint(user_id).to_le_bytes());
    out.extend_from_slice(&unix_time.to_le_bytes());
    let mut uid = [0u8; 0x81];
    let n = user_id.len().min(MAX_USER_ID_LEN);
    uid[..n].copy_from_slice(&user_id.as_bytes()[..n]);
    out.extend_from_slice(&uid);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_ring_fingerprint() {
        // Library values. A ring programmed with "tom-pc" advertises 0x79EE719F: only the low
        // 16 bits are compared (fingerprintMatchesUserId masks with 0xffff).
        assert_eq!(user_fingerprint(""), 0x6b4e_d927);
        assert_eq!(user_fingerprint("tom-pc"), 0x5ea8_719f);
        assert_eq!(build_app_data("a", 0).len(), APP_DATA_SIZE);
    }
}
