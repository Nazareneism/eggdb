use std::time::Duration;

use chrono::{TimeZone, Utc};

/// Format a byte count as a human-readable size string.
///
/// Uses binary-style thresholds but decimal-style labels (GB, MB, KB)
/// for consistency with common tools.
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Format a duration as a human-readable string.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs >= 60.0 {
        let mins = secs as u64 / 60;
        let remainder = secs - (mins as f64 * 60.0);
        format!("{mins}m {remainder:.2}s")
    } else {
        format!("{secs:.2}s")
    }
}

/// Format a UNIX timestamp as a human-readable UTC datetime.
pub fn format_timestamp(unix_secs: u64) -> String {
    match Utc.timestamp_opt(unix_secs as i64, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        _ => format!("{unix_secs} (invalid timestamp)"),
    }
}

/// Format a compression ratio from original and compressed sizes.
pub fn format_ratio(original: u64, compressed: u64) -> String {
    if compressed == 0 {
        return "N/A".into();
    }
    let ratio = original as f64 / compressed as f64;
    format!("{ratio:.1}x")
}

/// Truncate a display string to a maximum length, appending "..." if truncated.
///
/// Respects UTF-8 character boundaries -- truncation always lands on a valid
/// char boundary so this never panics on multi-byte characters.
pub fn truncate_display(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else if max_len <= 3 {
        "...".to_string()
    } else {
        let target = max_len - 3;
        // Find the largest char boundary <= target
        let end = s
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= target)
            .last()
            .unwrap_or(0);
        format!("{}...", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1536), "1.50 KB");
        assert_eq!(format_size(1048576), "1.00 MB");
        assert_eq!(format_size(1073741824), "1.00 GB");
        assert_eq!(format_size(1_500_000_000), "1.40 GB");
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(Duration::from_secs_f64(0.34)), "0.34s");
        assert_eq!(format_duration(Duration::from_secs_f64(12.22)), "12.22s");
        assert_eq!(format_duration(Duration::from_secs_f64(72.5)), "1m 12.50s");
    }

    #[test]
    fn test_format_ratio() {
        assert_eq!(format_ratio(1000, 200), "5.0x");
        assert_eq!(format_ratio(1500, 300), "5.0x");
        assert_eq!(format_ratio(0, 0), "N/A");
    }

    #[test]
    fn test_truncate_display() {
        assert_eq!(truncate_display("hello", 10), "hello");
        assert_eq!(truncate_display("hello world", 8), "hello...");
    }

    #[test]
    fn test_truncate_display_unicode() {
        // Multi-byte characters should not panic
        let greek = "\u{0395}\u{03C0}\u{0020}\u{03B1}\u{03C5}\u{03C4}";
        let result = truncate_display(greek, 5);
        assert!(result.ends_with("..."));
        // Should not panic even with very small max_len
        assert_eq!(truncate_display(greek, 3), "...");
        assert_eq!(truncate_display(greek, 0), "...");
    }

    #[test]
    fn test_truncate_display_emoji() {
        // Emoji are 4-byte UTF-8 sequences
        let s = "Hello \u{1F600} world";
        let result = truncate_display(s, 10);
        assert!(result.ends_with("..."));
        // Should never panic
        for max in 0..=s.len() + 5 {
            let _ = truncate_display(s, max);
        }
    }
}
