pub mod activity_page;
pub mod downloads_page;
pub mod models_page;
pub mod overview_page;
pub mod permissions_page;
pub mod runtimes_page;

pub(super) fn install_is_terminal_status(status: &str) -> bool {
    status.starts_with("Failed:") || status == "Completed"
}

pub(super) fn source_label(source: &str) -> &'static str {
    match source {
        "system" => "System",
        "user" => "User",
        _ => "Unknown source",
    }
}

pub(super) fn format_speed(bytes_per_second: i64) -> String {
    let bytes_per_second = bytes_per_second as f64;
    if bytes_per_second >= 1_000_000_000.0 {
        format!("{:.1} GB/s", bytes_per_second / 1_000_000_000.0)
    } else if bytes_per_second >= 1_000_000.0 {
        format!("{:.1} MB/s", bytes_per_second / 1_000_000.0)
    } else if bytes_per_second >= 1_000.0 {
        format!("{:.1} KB/s", bytes_per_second / 1_000.0)
    } else {
        format!("{} B/s", bytes_per_second as i64)
    }
}

pub(super) fn format_duration(seconds: i64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hegel::TestCase;
    use hegel::generators as gs;

    #[hegel::test]
    fn install_terminal_status_matches_generated_states(tc: TestCase) {
        let status = tc.draw(gs::sampled_from(vec![
            "Completed".to_string(),
            "Failed: network unavailable".to_string(),
            "Downloading model.gguf...".to_string(),
            "Preparing runtime image...".to_string(),
        ]));

        assert_eq!(
            install_is_terminal_status(&status),
            status == "Completed" || status.starts_with("Failed:")
        );
    }

    #[hegel::test]
    fn source_label_maps_generated_known_sources(tc: TestCase) {
        let (source, label) = tc.draw(gs::sampled_from(vec![
            ("system", "System"),
            ("user", "User"),
            ("other", "Unknown source"),
        ]));

        assert_eq!(source_label(source), label);
    }

    #[hegel::test]
    fn format_duration_uses_expected_generated_unit(tc: TestCase) {
        let seconds = tc.draw(gs::integers::<i64>().min_value(0).max_value(24 * 60 * 60));
        let formatted = format_duration(seconds);

        if seconds < 60 {
            assert_eq!(formatted, format!("{seconds}s"));
        } else if seconds < 3600 {
            assert_eq!(formatted, format!("{}m {}s", seconds / 60, seconds % 60));
        } else {
            assert_eq!(
                formatted,
                format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60)
            );
        }
    }

    #[hegel::test]
    fn format_speed_uses_expected_generated_unit(tc: TestCase) {
        let bytes_per_second = tc.draw(gs::integers::<i64>().min_value(0).max_value(2_000_000_000));
        let formatted = format_speed(bytes_per_second);

        if bytes_per_second >= 1_000_000_000 {
            assert!(formatted.ends_with(" GB/s"));
        } else if bytes_per_second >= 1_000_000 {
            assert!(formatted.ends_with(" MB/s"));
        } else if bytes_per_second >= 1_000 {
            assert!(formatted.ends_with(" KB/s"));
        } else {
            assert_eq!(formatted, format!("{bytes_per_second} B/s"));
        }
    }
}
