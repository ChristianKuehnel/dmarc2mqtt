mod config;
mod history;
mod imap_client;
mod mqtt;
mod parser;

use std::collections::BTreeMap;
use std::env;
use std::path::Path;
use std::process::ExitCode;
use std::str::FromStr;
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use cron::Schedule;
use env_logger::Env;
use log::{error, info};
use parser::ScanSummary;

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

    info!(
        "Starting daemon poll loop for IMAP folder '{}' with schedule '{}'.",
        config.imap.report_folder, config.imap.poll_cron
    );
    info!("History file path: {}", history_path.display());

    process_once(&config, &history_path);

    loop {
        let config_for_schedule = config::load_config(&args.config_path)?;
        let schedule = parse_schedule(&config_for_schedule.imap.poll_cron)?;
        let next_run = next_tick(&schedule)?;
        let wait = until(next_run);
        info!(
            "Next polling run at {} (in {}) using schedule '{}'.",
            next_run.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S %Z"),
            human_duration(wait),
            config_for_schedule.imap.poll_cron
        );
        thread::sleep(wait);

        let config_for_run = config::load_config(&args.config_path)?;
        process_once(&config_for_run, &history_path);
    }
}

fn parse_schedule(cron_expression: &str) -> Result<Schedule, String> {
    Schedule::from_str(cron_expression).map_err(|err| {
        format!("Config field imap.poll_cron is invalid ({cron_expression}): {err}")
    })
}

fn process_once(config: &config::AppConfig, history_path: &Path) {
    info!("Starting IMAP polling cycle.");

    if let Err(err) = process_once_inner(config, history_path) {
        error!("Polling cycle failed: {err}");
    }
}

fn process_once_inner(config: &config::AppConfig, history_path: &Path) -> Result<(), String> {
    let messages = imap_client::fetch_messages_with_attachments(config)?;
    let mut summaries = Vec::new();
    let mut message_uids_to_move = Vec::new();
    let mut moved_to_trash = 0usize;

    for message in messages {
        let parsed = parser::scan_report_inputs(&message.attachments, config.imap.max_xml_size)?;
        if parsed.is_empty() {
            continue;
        }

        summaries.extend(parsed);
        if config.imap.move_emails {
            message_uids_to_move.push(message.uid);
        }
    }

    let history_update = history::update_history(history_path, &summaries)?;
    if let Some(max_age_days) = config.mqtt.remove_stale_sensors {
        let prune_result = history::prune_stale_tuples(history_path, max_age_days)?;
        info!(
            "Pruned stale history tuples older than {} day(s): removed {}, remaining {}.",
            max_age_days, prune_result.removed_tuples, prune_result.remaining_tuples
        );
    }
    let known_tuples = history::load_known_tuples(history_path)?;
    let newest_seen_epoch = known_tuples.iter().map(|item| item.last_seen_epoch).max();
    info!(
        "Updated history file {}: {} tuple(s) seen in this poll, {} total stored.",
        history_path.display(),
        history_update.updated_tuples,
        history_update.total_tuples
    );
    info!(
        "Publishing Home Assistant discovery for {} tuple(s) from history (newest last_seen_epoch: {}).",
        known_tuples.len(),
        newest_seen_epoch.unwrap_or(0)
    );

    if summaries.is_empty() {
        info!(
            "No supported attachments found in configured IMAP folder. Expected .xml, .gz, or .zip attachments."
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

    if config.imap.move_emails {
        for uid in message_uids_to_move {
            imap_client::move_message_to_trash(config, uid)?;
            moved_to_trash += 1;
        }
    }

    info!("Published reports to MQTT.");
    if config.imap.move_emails {
        info!("Moved emails to trash: {moved_to_trash}");
    } else {
        info!("Move emails disabled (imap.move_emails=false).");
    }
    info!("Total documents: {}", summaries.len());

    Ok(())
}

fn next_tick(schedule: &Schedule) -> Result<DateTime<Utc>, String> {
    schedule
        .upcoming(Utc)
        .next()
        .ok_or_else(|| "Polling schedule has no upcoming execution time".to_owned())
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
