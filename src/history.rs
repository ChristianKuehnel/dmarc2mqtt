use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::parser::ScanSummary;

#[derive(Debug, Clone, Copy)]
pub(crate) struct HistoryUpdateResult {
    pub(crate) updated_tuples: usize,
    pub(crate) total_tuples: usize,
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
) -> Result<HistoryUpdateResult, String> {
    let mut history_map = load_history(history_path)?;
    let now_epoch = Utc::now().timestamp();
    let mut updated_keys: BTreeSet<(String, String)> = BTreeSet::new();

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
        history_map.insert(key.clone(), now_epoch);
        updated_keys.insert(key);
    }

    save_history(history_path, &history_map)?;

    Ok(HistoryUpdateResult {
        updated_tuples: updated_keys.len(),
        total_tuples: history_map.len(),
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
