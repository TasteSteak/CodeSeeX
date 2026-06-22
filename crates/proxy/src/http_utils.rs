use codeseex_core::AppConfig;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0)
}

pub(crate) fn normalize_version_label(version: &str) -> String {
    version
        .trim()
        .trim_start_matches(['v', 'V'])
        .trim()
        .to_owned()
}

pub(crate) fn is_newer_version(latest: &str, current: &str) -> bool {
    let latest_parts = version_parts(latest);
    let current_parts = version_parts(current);
    for index in 0..latest_parts.len().max(current_parts.len()) {
        let latest = *latest_parts.get(index).unwrap_or(&0);
        let current = *current_parts.get(index).unwrap_or(&0);
        if latest != current {
            return latest > current;
        }
    }
    false
}

fn version_parts(version: &str) -> Vec<u64> {
    normalize_version_label(version)
        .split('.')
        .map(|part| {
            part.chars()
                .take_while(|ch| ch.is_ascii_digit())
                .collect::<String>()
                .parse::<u64>()
                .unwrap_or(0)
        })
        .collect()
}

pub(crate) fn config_version(config: &AppConfig) -> String {
    std::fs::metadata(config.config_path())
        .ok()
        .and_then(|metadata| {
            let len = metadata.len();
            metadata.modified().ok().and_then(|modified| {
                modified
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .map(|duration| format!("{}-{}", duration.as_nanos(), len))
            })
        })
        .unwrap_or_else(|| "0".to_owned())
}

pub(crate) fn io_result<T>(value: T) -> Result<T, std::io::Error> {
    Ok(value)
}
