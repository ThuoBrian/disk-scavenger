//! disk-scavenger core — CLI, TUI, and GUI frontends all compile this as their module tree.

use anyhow::Context;

pub mod actions;
pub mod scanner;
pub mod transfer;
pub mod tui;
pub mod views;

/// Read buffer for SHA256 hashing (scanner dedup + transfer verification)
const HASH_BUF: usize = 64 * 1024;

/// SHA256 hex digest of a file's contents.
pub fn hash_file(path: &std::path::Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .with_context(|| format!("Cannot open for hashing: {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; HASH_BUF];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("Hash read error: {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Home directory for both platforms: $HOME (unix) or %USERPROFILE% (Windows).
// lean-dev: no fallback to "/" on Windows — temp dir is the only safe default there
pub fn home_dir() -> std::path::PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_or_else(|_| std::env::temp_dir(), std::path::PathBuf::from)
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{:.1} {}", size, UNITS[unit])
}

/// Parse a size like `500MB`, `1.5GiB` or `1024` (binary units throughout).
pub fn parse_size_arg(s: &str) -> Result<u64, String> {
    let s = s.trim().to_uppercase();
    let (num_part, unit_part) = match s.find(|c: char| c.is_alphabetic()) {
        Some(i) => (s[..i].trim(), &s[i..]),
        None => (s.as_str(), ""),
    };
    let mult: u64 = match unit_part {
        "" | "B" => 1,
        "K" | "KB" | "KIB" => 1024,
        "M" | "MB" | "MIB" => 1024 * 1024,
        "G" | "GB" | "GIB" => 1024u64 * 1024 * 1024,
        "T" | "TB" | "TIB" => 1024u64 * 1024 * 1024 * 1024,
        _ => return Err(format!("unknown size unit: {unit_part}")),
    };
    if let Ok(n) = num_part.parse::<u64>() {
        return n
            .checked_mul(mult)
            .ok_or_else(|| format!("size too large: {n}{unit_part}"));
    }
    let n: f64 = num_part
        .parse()
        .map_err(|_| format!("invalid number: {num_part}"))?;
    let bytes = n * mult as f64;
    // u64::MAX as f64 rounds up to 2^64, so `<` keeps the cast in range
    if !bytes.is_finite() || bytes < 0.0 || bytes >= u64::MAX as f64 {
        return Err(format!("invalid size: {num_part}{unit_part}"));
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // range-checked above
    Ok(bytes.round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_human_size_bytes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1023), "1023 B");
    }

    #[test]
    fn test_human_size_kib() {
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(1536), "1.5 KiB");
    }

    #[test]
    fn test_human_size_mib() {
        assert_eq!(human_size(1_048_576), "1.0 MiB");
        assert_eq!(human_size(524_288_000), "500.0 MiB");
    }

    #[test]
    fn test_human_size_gib() {
        assert_eq!(human_size(1_073_741_824), "1.0 GiB");
    }

    #[test]
    fn test_parse_size_arg() {
        assert_eq!(parse_size_arg("500MB").unwrap(), 524_288_000);
        assert_eq!(parse_size_arg("1GB").unwrap(), 1_073_741_824);
        assert_eq!(parse_size_arg("1024").unwrap(), 1024);
        assert_eq!(parse_size_arg("2K").unwrap(), 2048);
        assert_eq!(parse_size_arg("1GiB").unwrap(), 1_073_741_824);
        assert_eq!(parse_size_arg("1.5GB").unwrap(), 1_610_612_736);
        assert_eq!(parse_size_arg("0.5 KB").unwrap(), 512);
    }

    #[test]
    fn test_hash_file() {
        // Well-known SHA-256 of empty input
        let tmp = tempfile::NamedTempFile::new().unwrap();
        assert_eq!(
            hash_file(tmp.path()).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_parse_size_arg_invalid() {
        assert!(parse_size_arg("abc").is_err());
        assert!(parse_size_arg("500XB").is_err());
        assert!(parse_size_arg("-1GB").is_err());
        assert!(parse_size_arg("NaNGB").is_err());
    }
}
