mod config;
mod history;
mod imap_client;
mod mqtt;
mod parser;

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::Path;
use std::process::ExitCode;
use std::str::FromStr;
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use cron::Schedule;
use config::{AppConfig, ImapMailboxConfig};
use env_logger::Env;
use log::{error, info, warn};
use parser::{ScanSummary, ZipLimits};

fn main() -> ExitCode {
    init_logger();

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            if err.is_empty() {
                ExitCode::SUCCESS
            } else {
                error!("{err}");
                ExitCode::FAILURE
            }
        }
    }
}

fn init_logger() {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let config = config::load_config(&args.config_path)?;
    let history_path = history::history_path_for_config(&args.config_path);
    let mut mailbox_last_run = BTreeMap::new();

    info!(
        "Starting daemon poll loop for {} configured IMAP mailbox(es).",
        config.imap.mailboxes.len()
    );
    for mailbox in &config.imap.mailboxes {
        info!(
            "Configured mailbox '{}': {}:{} folder '{}' schedule '{}'.",
            mailbox.name,
            mailbox.server_name,
            mailbox.server_port,
            mailbox.report_folder,
            mailbox.poll_cron
        );
    }
    info!("History file path: {}", history_path.display());

    process_mailboxes(&config, &history_path, &config.imap.mailboxes, &mut mailbox_last_run);

    loop {
        let config_for_schedule = config::load_config(&args.config_path)?;
        let next_run = next_due_mailbox_run(&config_for_schedule, &mailbox_last_run)?;
        let wait = until(next_run);
        info!(
            "Next polling run at {} (in {}).",
            next_run.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S %Z"),
            human_duration(wait),
        );
        thread::sleep(wait);

        let config_for_run = config::load_config(&args.config_path)?;
        let due_mailboxes = due_mailboxes(&config_for_run, &mailbox_last_run, Utc::now())?;
        if due_mailboxes.is_empty() {
            continue;
        }
        process_mailboxes(
            &config_for_run,
            &history_path,
            &due_mailboxes,
            &mut mailbox_last_run,
        );
    }
}

fn parse_schedule(cron_expression: &str) -> Result<Schedule, String> {
    Schedule::from_str(cron_expression)
        .map_err(|err| format!("Invalid poll_cron ({cron_expression}): {err}"))
}

fn process_mailboxes(
    config: &AppConfig,
    history_path: &Path,
    mailboxes: &[ImapMailboxConfig],
    mailbox_last_run: &mut BTreeMap<String, DateTime<Utc>>,
) {
    info!(
        "Starting IMAP polling cycle for mailbox(es): {}.",
        mailbox_names(mailboxes)
    );

    if let Err(err) = process_mailboxes_inner(config, history_path, mailboxes, mailbox_last_run) {
        error!("Polling cycle failed: {err}");
    }
}

fn process_mailboxes_inner(
    config: &AppConfig,
    history_path: &Path,
    mailboxes: &[ImapMailboxConfig],
    mailbox_last_run: &mut BTreeMap<String, DateTime<Utc>>,
) -> Result<(), String> {
    let mut summaries = Vec::new();
    let mut message_uids_to_move: Vec<(String, u32)> = Vec::new();
    let mut moved_to_trash = 0usize;

    for mailbox in mailboxes {
        info!(
            "Polling mailbox '{}': {}:{} folder '{}'.",
            mailbox.name, mailbox.server_name, mailbox.server_port, mailbox.report_folder
        );

        let messages = imap_client::fetch_messages_with_attachments(mailbox)?;
        for message in messages {
            let parsed = parser::scan_report_inputs(
                &message.attachments,
                mailbox.max_xml_size,
                ZipLimits {
                    max_entries: mailbox.max_zip_entries,
                    max_xml_files: mailbox.max_zip_xml_files,
                    max_uncompressed_size_mb: mailbox.max_zip_uncompressed_size,
                },
            )?;
            if parsed.is_empty() {
                continue;
            }

            summaries.extend(filter_report_summaries(mailbox, parsed));
            if mailbox.move_emails {
                message_uids_to_move.push((mailbox.name.clone(), message.uid));
            }
        }

        mailbox_last_run.insert(mailbox.name.clone(), Utc::now());
    }

    if let Some(max_age_days) = config.mqtt.remove_stale_sensors {
        let prune_result = history::prune_stale_tuples(history_path, max_age_days)?;
        info!(
            "Pruned stale history tuples older than {} day(s): removed {}, remaining {}.",
            max_age_days, prune_result.removed_tuples, prune_result.remaining_tuples
        );
    }
    let history_update =
        history::update_history(history_path, &summaries, config.mqtt.max_history_entries)?;
    let summaries =
        filter_summaries_by_accepted_history(summaries, &history_update.accepted_tuples);
    let known_tuples = history::load_known_tuples(history_path)?;
    let newest_seen_epoch = known_tuples.iter().map(|item| item.last_seen_epoch).max();
    info!(
        "Updated history file {}: {} tuple(s) seen in this poll, {} total stored.",
        history_path.display(),
        history_update.updated_tuples,
        history_update.total_tuples
    );
    if history_update.skipped_new_tuples > 0 {
        warn!(
            "Skipped {} new tuple(s) because mqtt.max_history_entries ({}) is full.",
            history_update.skipped_new_tuples, config.mqtt.max_history_entries
        );
    }
    if history_update.removed_over_limit_tuples > 0 {
        warn!(
            "Removed {} old history tuple(s) to enforce mqtt.max_history_entries ({}).",
            history_update.removed_over_limit_tuples, config.mqtt.max_history_entries
        );
    }
    info!(
        "Publishing Home Assistant discovery for {} tuple(s) from history (newest last_seen_epoch: {}).",
        known_tuples.len(),
        newest_seen_epoch.unwrap_or(0)
    );

    if summaries.is_empty() {
        info!(
            "No supported attachments found in the polled IMAP mailboxes. Expected .xml, .gz, or .zip attachments."
        );
    }

    for summary in &summaries {
        info!("Scanned: {}", summary.source);
        info!(
            "  org_name: {}",
            summary.org_name.as_deref().unwrap_or("<missing>")
        );
        info!("  email: {}", summary.email.as_deref().unwrap_or("<missing>"));
        info!(
            "  policy_published/domain: {}",
            summary
                .policy_published_domain
                .as_deref()
                .unwrap_or("<missing>")
        );
        info!("  result/pass: {}", summary.result_pass_count);
        info!("  result/fail: {}", summary.result_fail_count);
    }

    let aggregated_statuses = aggregate_statuses(&summaries);
    let domain_statuses = aggregate_domain_statuses(&summaries);
    print_aggregated_status(&aggregated_statuses);
    print_domain_status(&domain_statuses);

    mqtt::publish_reports_to_mqtt(config, &aggregated_statuses, &domain_statuses, &known_tuples)?;

    for (mailbox_name, uid) in message_uids_to_move {
        let mailbox = config
            .imap
            .mailboxes
            .iter()
            .find(|mailbox| mailbox.name == mailbox_name)
            .ok_or_else(|| format!("Configured mailbox '{}' disappeared during processing", mailbox_name))?;
        if mailbox.move_emails {
            imap_client::move_message_to_trash(mailbox, uid)?;
            moved_to_trash += 1;
        }
    }

    info!("Published reports to MQTT.");
    if mailboxes.iter().any(|mailbox| mailbox.move_emails) {
        info!("Moved emails to trash: {moved_to_trash}");
    } else {
        info!("Move emails disabled for the processed mailboxes.");
    }
    info!("Total documents: {}", summaries.len());

    Ok(())
}

fn next_tick_after(schedule: &Schedule, reference: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    schedule.after(&reference).next().ok_or_else(|| {
        "Polling schedule has no upcoming execution time".to_owned()
    })
}

fn next_due_mailbox_run(
    config: &AppConfig,
    mailbox_last_run: &BTreeMap<String, DateTime<Utc>>,
) -> Result<DateTime<Utc>, String> {
    let now = Utc::now();
    let mut next_runs = Vec::new();

    for mailbox in &config.imap.mailboxes {
        match mailbox_last_run.get(&mailbox.name).copied() {
            Some(last_run) => {
                let schedule = parse_schedule(&mailbox.poll_cron)?;
                next_runs.push(next_tick_after(&schedule, last_run)?);
            }
            None => return Ok(now),
        }
    }

    next_runs
        .into_iter()
        .min()
        .ok_or_else(|| "No IMAP mailboxes configured".to_owned())
}

fn due_mailboxes(
    config: &AppConfig,
    mailbox_last_run: &BTreeMap<String, DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<Vec<ImapMailboxConfig>, String> {
    let mut due = Vec::new();

    for mailbox in &config.imap.mailboxes {
        let Some(last_run) = mailbox_last_run.get(&mailbox.name).copied() else {
            due.push(mailbox.clone());
            continue;
        };

        let schedule = parse_schedule(&mailbox.poll_cron)?;
        let next_run = next_tick_after(&schedule, last_run)?;
        if next_run <= now {
            due.push(mailbox.clone());
        }
    }

    Ok(due)
}

fn mailbox_names(mailboxes: &[ImapMailboxConfig]) -> String {
    mailboxes
        .iter()
        .map(|mailbox| mailbox.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn filter_report_summaries(
    mailbox: &ImapMailboxConfig,
    summaries: Vec<ScanSummary>,
) -> Vec<ScanSummary> {
    summaries
        .into_iter()
        .filter(|summary| report_summary_allowed(mailbox, summary))
        .collect()
}

fn report_summary_allowed(mailbox: &ImapMailboxConfig, summary: &ScanSummary) -> bool {
    let Some(org_name) = summary.org_name.as_deref().map(str::trim) else {
        warn!(
            "Skipping DMARC report {}: missing report_metadata/org_name.",
            summary.source
        );
        return false;
    };
    let Some(domain) = summary.policy_published_domain.as_deref().map(str::trim) else {
        warn!(
            "Skipping DMARC report {}: missing policy_published/domain.",
            summary.source
        );
        return false;
    };
    if org_name.is_empty() || domain.is_empty() {
        warn!(
            "Skipping DMARC report {}: empty org_name or domain.",
            summary.source
        );
        return false;
    }
    if org_name.chars().count() > mailbox.max_report_org_name_length {
        warn!(
            "Skipping DMARC report {}: org_name exceeds max_report_org_name_length ({}).",
            summary.source, mailbox.max_report_org_name_length
        );
        return false;
    }
    if domain.len() > mailbox.max_report_domain_length {
        warn!(
            "Skipping DMARC report {}: domain exceeds max_report_domain_length ({}).",
            summary.source, mailbox.max_report_domain_length
        );
        return false;
    }
    true
}

fn filter_summaries_by_accepted_history(
    summaries: Vec<ScanSummary>,
    accepted_tuples: &BTreeSet<(String, String)>,
) -> Vec<ScanSummary> {
    summaries
        .into_iter()
        .filter(|summary| {
            let Some(key) = summary_tuple_key(summary) else {
                return false;
            };
            accepted_tuples.contains(&key)
        })
        .collect()
}

fn summary_tuple_key(summary: &ScanSummary) -> Option<(String, String)> {
    let org_name = summary.org_name.as_deref()?.trim();
    let domain = summary.policy_published_domain.as_deref()?.trim();
    if org_name.is_empty() || domain.is_empty() {
        None
    } else {
        Some((org_name.to_owned(), domain.to_owned()))
    }
}

fn until(next_run: DateTime<Utc>) -> Duration {
    let now = Utc::now();
    if next_run <= now {
        return Duration::from_secs(0);
    }

    let delta = next_run.signed_duration_since(now);
    delta.to_std().unwrap_or_else(|_| Duration::from_secs(0))
}

fn human_duration(duration: Duration) -> String {
    let total = duration.as_secs();
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    format!("{hours}h {minutes}m {seconds}s")
}

struct CliArgs {
    config_path: String,
}

fn parse_args() -> Result<CliArgs, String> {
    let mut args = env::args().skip(1);
    let mut config_path = "config.yaml".to_owned();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" | "-c" => {
                config_path = args.next().ok_or_else(|| {
                    "Missing value for --config. Usage: dmarc2mqtt [--config <PATH>]".to_owned()
                })?;
            }
            "--help" | "-h" => {
                print_help();
                return Err(String::new());
            }
            _ => {
                return Err(format!(
                    "Unknown argument: {arg}\nUsage: dmarc2mqtt [--config <PATH>]"
                ));
            }
        }
    }

    Ok(CliArgs { config_path })
}

fn print_help() {
    println!("Usage: dmarc2mqtt [--config <PATH>]");
    println!();
    println!("Options:");
    println!("  -c, --config <PATH>      YAML config file path (default: config.yaml)");
    println!("  -h, --help               Show this help message");
}

#[derive(Debug, Clone)]
pub(crate) struct AggregatedStatus {
    pub(crate) org_name: String,
    pub(crate) domain: String,
    pub(crate) status: String,
}

#[derive(Debug, Clone)]
pub(crate) struct DomainStatus {
    pub(crate) domain: String,
    pub(crate) status: String,
}

fn aggregate_statuses(summaries: &[ScanSummary]) -> Vec<AggregatedStatus> {
    let mut grouped: BTreeMap<(String, String), (usize, usize)> = BTreeMap::new();

    for summary in summaries {
        let org_name = summary.org_name.as_deref().unwrap_or("<missing>").to_owned();
        let domain = summary
            .policy_published_domain
            .as_deref()
            .unwrap_or("<missing>")
            .to_owned();

        let entry = grouped.entry((org_name, domain)).or_insert((0, 0));
        entry.0 += summary.result_pass_count;
        entry.1 += summary.result_fail_count;
    }

    let mut statuses = Vec::new();
    for ((org_name, domain), (pass_count, fail_count)) in grouped {
        let total = pass_count + fail_count;
        let status = if fail_count == 0 {
            "pass".to_owned()
        } else {
            let percent_failed = (fail_count as f64 / total as f64) * 100.0;
            format!("{percent_failed:.1}% failed")
        };

        statuses.push(AggregatedStatus {
            org_name,
            domain,
            status,
        });
    }

    statuses
}

fn print_aggregated_status(statuses: &[AggregatedStatus]) {
    info!("Aggregated status:");
    for item in statuses {
        info!("  {} / {}: {}", item.org_name, item.domain, item.status);
    }
}

fn aggregate_domain_statuses(summaries: &[ScanSummary]) -> Vec<DomainStatus> {
    let mut grouped: BTreeMap<String, (usize, usize)> = BTreeMap::new();

    for summary in summaries {
        let domain = summary
            .policy_published_domain
            .as_deref()
            .unwrap_or("<missing>")
            .to_owned();

        let entry = grouped.entry(domain).or_insert((0, 0));
        entry.0 += summary.result_pass_count;
        entry.1 += summary.result_fail_count;
    }

    let mut statuses = Vec::new();
    for (domain, (pass_count, fail_count)) in grouped {
        let total = pass_count + fail_count;
        let status = if fail_count == 0 {
            "pass".to_owned()
        } else {
            let percent_failed = (fail_count as f64 / total as f64) * 100.0;
            format!("{percent_failed:.1}% failed")
        };

        statuses.push(DomainStatus { domain, status });
    }

    statuses
}

fn print_domain_status(statuses: &[DomainStatus]) {
    info!("Domain totals:");
    for item in statuses {
        info!("  {}: {}", item.domain, item.status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mailbox() -> ImapMailboxConfig {
        ImapMailboxConfig {
            name: "primary".to_owned(),
            server_name: "imap.example.com".to_owned(),
            server_port: 993,
            login: "user@example.com".to_owned(),
            password: "change-me".to_owned(),
            report_folder: "INBOX/DMARC".to_owned(),
            trash_folder: "Trash".to_owned(),
            move_emails: false,
            poll_cron: "0 0 */6 * * *".to_owned(),
            max_xml_size: 10,
            max_message_size: 25,
            max_attachment_size: 10,
            max_zip_entries: 1000,
            max_zip_xml_files: 10,
            max_zip_uncompressed_size: None,
            max_report_org_name_length: 16,
            max_report_domain_length: 253,
        }
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
    fn filters_reports_outside_metadata_limits() {
        let mailbox = mailbox();
        let allowed = summary("Reporter", "example.com");
        let long_org = summary("Reporter With A Very Long Name", "example.com");

        let filtered = filter_report_summaries(&mailbox, vec![allowed, long_org]);

        assert_eq!(filtered.len(), 1);
        assert_eq!(
            filtered[0].policy_published_domain.as_deref(),
            Some("example.com")
        );
    }
}
