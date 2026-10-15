//! WAV output. Not part of the Haversine library: replicates the phone app's DC-bias
//! removal (coredevices.haversine.removeDCBias, `vendor/java_src/coredevices/haversine/UtilKt.java`)
//! and writes a standard 16-bit mono PCM file.

use std::path::Path;

/// Build a 16-bit mono PCM WAV file in memory.
///
/// Matches the app (coredevices.haversine.removeDCBias in UtilKt.java): treat samples as i16,
/// take every 80th sample (excluding the last), round their mean half-to-even, subtract it.
pub fn wav_bytes(samples: &[u16], rate: u32) -> Vec<u8> {
    let vals: Vec<i32> = samples.iter().map(|&s| s as i16 as i32).collect();
    let end = vals.len().saturating_sub(1);
    let picked: Vec<i64> = (0..end).step_by(80).map(|i| vals[i] as i64).collect();
    let mean = if picked.is_empty() {
        0
    } else {
        (picked.iter().sum::<i64>() as f64 / picked.len() as f64).round_ties_even() as i32
    };

    let data_len = (vals.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for v in vals {
        out.extend_from_slice(&((v - mean) as u16).to_le_bytes());
    }
    out
}

pub fn write_wav(samples: &[u16], rate: u32, path: impl AsRef<Path>) -> std::io::Result<()> {
    std::fs::write(path, wav_bytes(samples, rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header() {
        let w = wav_bytes(&[1, 2, 3], 16000);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(w.len(), 44 + 6);
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 16000);
    }
}
