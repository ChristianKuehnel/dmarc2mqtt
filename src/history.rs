use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::parser::ScanSummary;

#[derive(Debug, Clone)]
pub(crate) struct HistoryUpdateResult {
    pub(crate) updated_tuples: usize,
    pub(crate) total_tuples: usize,
    pub(crate) skipped_new_tuples: usize,
    pub(crate) removed_over_limit_tuples: usize,
    pub(crate) accepted_tuples: BTreeSet<(String, String)>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PruneResult {
    pub(crate) removed_tuples: usize,
    pub(crate) remaining_tuples: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct KnownTuple {
    pub(crate) org_name: String,
    pub(crate) domain: String,
    pub(crate) last_seen_epoch: i64,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct HistoryFile {
    entries: Vec<HistoryEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct HistoryEntry {
    org_name: String,
    domain: String,
    last_seen_epoch: i64,
}

pub(crate) fn history_path_for_config(config_path: &str) -> PathBuf {
    let config = Path::new(config_path);
    let parent = config.parent().unwrap_or_else(|| Path::new("."));
    parent.join("history.json")
}

pub(crate) fn update_history(
    history_path: &Path,
    summaries: &[ScanSummary],
    max_entries: usize,
) -> Result<HistoryUpdateResult, String> {
    let mut history_map = load_history(history_path)?;
    let removed_over_limit_tuples = trim_history_to_max_entries(&mut history_map, max_entries);
    let now_epoch = Utc::now().timestamp();
    let mut updated_keys: BTreeSet<(String, String)> = BTreeSet::new();
    let mut skipped_new_keys: BTreeSet<(String, String)> = BTreeSet::new();

    for summary in summaries {
        let Some(org_name) = summary.org_name.as_deref() else {
            continue;
        };
        let Some(domain) = summary.policy_published_domain.as_deref() else {
            continue;
        };

        let org_name = org_name.trim();
        let domain = domain.trim();
        if org_name.is_empty() || domain.is_empty() {
            continue;
        }

        let key = (org_name.to_owned(), domain.to_owned());
        if !history_map.contains_key(&key) && history_map.len() >= max_entries {
            skipped_new_keys.insert(key);
            continue;
        }
        history_map.insert(key.clone(), now_epoch);
        updated_keys.insert(key);
    }

    save_history(history_path, &history_map)?;

    Ok(HistoryUpdateResult {
        updated_tuples: updated_keys.len(),
        total_tuples: history_map.len(),
        skipped_new_tuples: skipped_new_keys.len(),
        removed_over_limit_tuples,
        accepted_tuples: updated_keys,
    })
}

pub(crate) fn load_known_tuples(history_path: &Path) -> Result<Vec<KnownTuple>, String> {
    let history_map = load_history(history_path)?;
    let mut out = Vec::with_capacity(history_map.len());
    for ((org_name, domain), last_seen_epoch) in history_map {
        out.push(KnownTuple {
            org_name,
            domain,
            last_seen_epoch,
        });
    }
    Ok(out)
}

pub(crate) fn prune_stale_tuples(history_path: &Path, max_age_days: u64) -> Result<PruneResult, String> {
    let mut history_map = load_history(history_path)?;
    let before_len = history_map.len();
    let now_epoch = Utc::now().timestamp();
    let max_age_seconds = i64::try_from(max_age_days)
        .map_err(|_| format!("remove_stale_sensors value is too large: {max_age_days}"))?
        .saturating_mul(86_400);
    let cutoff_epoch = now_epoch.saturating_sub(max_age_seconds);

    history_map.retain(|_, last_seen_epoch| *last_seen_epoch >= cutoff_epoch);
    save_history(history_path, &history_map)?;

    let remaining_tuples = history_map.len();
    Ok(PruneResult {
        removed_tuples: before_len.saturating_sub(remaining_tuples),
        remaining_tuples,
    })
}

fn load_history(path: &Path) -> Result<BTreeMap<(String, String), i64>, String> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }

    let content = fs::read_to_string(path)
        .map_err(|err| format!("Failed to read history file {}: {err}", path.display()))?;

    if content.trim().is_empty() {
        return Ok(BTreeMap::new());
    }

    let parsed: HistoryFile = serde_json::from_str(&content)
        .map_err(|err| format!("Invalid JSON in history file {}: {err}", path.display()))?;

    let mut map = BTreeMap::new();
    for entry in parsed.entries {
        map.insert((entry.org_name, entry.domain), entry.last_seen_epoch);
    }

    Ok(map)
}

fn save_history(path: &Path, history: &BTreeMap<(String, String), i64>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            format!(
                "Failed to create history directory {}: {err}",
                parent.display()
            )
        })?;
    }

    let mut entries = Vec::with_capacity(history.len());
    for ((org_name, domain), last_seen_epoch) in history {
        entries.push(HistoryEntry {
            org_name: org_name.clone(),
            domain: domain.clone(),
            last_seen_epoch: *last_seen_epoch,
        });
    }

    let file = HistoryFile { entries };
    let mut content = serde_json::to_string_pretty(&file)
        .map_err(|err| format!("Failed to serialize history file {}: {err}", path.display()))?;
    content.push('\n');

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("history.json");
    let tmp_path = path.with_file_name(format!("{file_name}.tmp"));

    fs::write(&tmp_path, content)
        .map_err(|err| format!("Failed to write temp history file {}: {err}", tmp_path.display()))?;
    fs::rename(&tmp_path, path).map_err(|err| {
        format!(
            "Failed to replace history file {} with {}: {err}",
            path.display(),
            tmp_path.display()
        )
    })?;

    Ok(())
}

fn trim_history_to_max_entries(
    history: &mut BTreeMap<(String, String), i64>,
    max_entries: usize,
) -> usize {
    let mut removed = 0usize;
    while history.len() > max_entries {
        let Some(key) = history
            .iter()
            .min_by_key(|(_, last_seen_epoch)| *last_seen_epoch)
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        history.remove(&key);
        removed += 1;
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_history_path() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "dmarc2mqtt-history-test-{}-{unique}.json",
            std::process::id()
        ))
    }

    fn summary(org_name: &str, domain: &str) -> ScanSummary {
        ScanSummary {
            source: format!("fixture:{org_name}-{domain}.xml"),
            org_name: Some(org_name.to_owned()),
            email: Some("noreply@example.com".to_owned()),
            policy_published_domain: Some(domain.to_owned()),
            result_pass_count: 1,
            result_fail_count: 0,
        }
    }

    #[test]
    fn caps_new_history_entries_without_evicting_existing_entries() {
        let path = temp_history_path();

        let first = update_history(&path, &[summary("Reporter A", "example.com")], 1)
            .expect("first history update should fit");
        assert_eq!(first.updated_tuples, 1);
        assert_eq!(first.total_tuples, 1);
        assert_eq!(first.skipped_new_tuples, 0);
        assert_eq!(first.removed_over_limit_tuples, 0);

        let second = update_history(
            &path,
            &[
                summary("Reporter A", "example.com"),
                summary("Reporter B", "example.net"),
            ],
            1,
        )
        .expect("history update should skip new overflow tuple");

        assert_eq!(second.updated_tuples, 1);
        assert_eq!(second.total_tuples, 1);
        assert_eq!(second.skipped_new_tuples, 1);
        assert_eq!(second.removed_over_limit_tuples, 0);
        assert!(second
            .accepted_tuples
            .contains(&("Reporter A".to_owned(), "example.com".to_owned())));

        let known = load_known_tuples(&path).expect("history should load");
        assert_eq!(known.len(), 1);
        assert_eq!(known[0].org_name, "Reporter A");
        assert_eq!(known[0].domain, "example.com");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn trims_existing_history_back_to_the_entry_limit() {
        let path = temp_history_path();
        let mut existing = BTreeMap::new();
        existing.insert(("Old Reporter".to_owned(), "old.example.com".to_owned()), 1);
        existing.insert(("New Reporter".to_owned(), "new.example.com".to_owned()), 2);
        save_history(&path, &existing).expect("history fixture should save");

        let result = update_history(&path, &[], 1).expect("history trim should succeed");

        assert_eq!(result.updated_tuples, 0);
        assert_eq!(result.total_tuples, 1);
        assert_eq!(result.removed_over_limit_tuples, 1);

        let known = load_known_tuples(&path).expect("history should load");
        assert_eq!(known.len(), 1);
        assert_eq!(known[0].org_name, "New Reporter");
        assert_eq!(known[0].domain, "new.example.com");

        let _ = fs::remove_file(path);
    }
}
