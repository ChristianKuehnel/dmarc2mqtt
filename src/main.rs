mod imap_client;
mod mqtt;
mod parser;

use std::collections::BTreeMap;
use std::env;
use std::process::ExitCode;

use parser::ScanSummary;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            if err.is_empty() {
                ExitCode::SUCCESS
            } else {
                eprintln!("{err}");
                ExitCode::FAILURE
            }
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let config = mqtt::load_config(&args.config_path)?;

    let messages = imap_client::fetch_messages_with_attachments(&config)?;
    let mut summaries = Vec::new();
    let mut moved_to_trash = 0usize;

    for message in messages {
        let parsed = parser::scan_report_inputs(&message.attachments)?;
        if parsed.is_empty() {
            continue;
        }

        summaries.extend(parsed);
        if config.imap.move_emails {
            imap_client::move_message_to_trash(&config, message.uid)?;
            moved_to_trash += 1;
        }
    }

    if summaries.is_empty() {
        return Err(
            "No supported attachments found in configured IMAP folder. Expected .xml, .gz, or .zip attachments."
                .to_owned(),
        );
    }

    for summary in &summaries {
        println!("Scanned: {}", summary.source);
        println!(
            "  org_name: {}",
            summary.org_name.as_deref().unwrap_or("<missing>")
        );
        println!("  email: {}", summary.email.as_deref().unwrap_or("<missing>"));
        println!(
            "  policy_published/domain: {}",
            summary
                .policy_published_domain
                .as_deref()
                .unwrap_or("<missing>")
        );
        println!("  result/pass: {}", summary.result_pass_count);
        println!("  result/fail: {}", summary.result_fail_count);
    }

    let aggregated_statuses = aggregate_statuses(&summaries);
    let domain_statuses = aggregate_domain_statuses(&summaries);
    print_aggregated_status(&aggregated_statuses);
    print_domain_status(&domain_statuses);

    mqtt::publish_reports_to_mqtt(&config, &aggregated_statuses, &domain_statuses)?;

    println!("Published reports to MQTT.");
    if config.imap.move_emails {
        println!("Moved emails to trash: {moved_to_trash}");
    } else {
        println!("Move emails disabled (imap.move_emails=false).");
    }
    println!("Total documents: {}", summaries.len());

    Ok(())
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
                config_path = args
                    .next()
                    .ok_or_else(|| {
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
    println!("Aggregated status:");
    for item in statuses {
        println!("  {} / {}: {}", item.org_name, item.domain, item.status);
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
    println!("Domain totals:");
    for item in statuses {
        println!("  {}: {}", item.domain, item.status);
    }
}
