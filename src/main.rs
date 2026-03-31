mod mqtt;
mod parser;

use std::collections::BTreeMap;
use std::env;
use std::path::Path;
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
    let summaries = parser::scan_directory(&args.directory_path)?;

    if summaries.is_empty() {
        return Err(format!(
            "No supported files found in {}. Expected .xml, .gz, or .zip.",
            args.directory_path
        ));
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
    print_aggregated_status(&aggregated_statuses);

    mqtt::publish_reports_to_mqtt(&config, &summaries, &aggregated_statuses)?;

    println!("Published reports to MQTT.");
    println!("Total documents: {}", summaries.len());

    Ok(())
}

struct CliArgs {
    directory_path: String,
    config_path: String,
}

fn parse_args() -> Result<CliArgs, String> {
    let mut args = env::args().skip(1);
    let mut directory_path: Option<String> = None;
    let mut config_path = "config.yaml".to_owned();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--directory" | "-d" => {
                let value = args
                    .next()
                    .ok_or_else(|| {
                        "Missing value for --directory. Usage: dmarc2mqtt --directory <PATH>"
                            .to_owned()
                    })?;
                directory_path = Some(value);
            }
            "--config" | "-c" => {
                config_path = args
                    .next()
                    .ok_or_else(|| {
                        "Missing value for --config. Usage: dmarc2mqtt --directory <PATH> [--config <PATH>]"
                            .to_owned()
                    })?;
            }
            "--help" | "-h" => {
                print_help();
                return Err(String::new());
            }
            _ => {
                return Err(format!(
                    "Unknown argument: {arg}\nUsage: dmarc2mqtt --directory <PATH> [--config <PATH>]"
                ));
            }
        }
    }

    let directory_path =
        directory_path.ok_or_else(|| "Missing required argument: --directory <PATH>".to_owned())?;

    if !Path::new(&directory_path).is_dir() {
        return Err(format!("Not a directory: {directory_path}"));
    }

    Ok(CliArgs {
        directory_path,
        config_path,
    })
}

fn print_help() {
    println!("Usage: dmarc2mqtt --directory <PATH> [--config <PATH>]");
    println!();
    println!("Options:");
    println!("  -d, --directory <PATH>   Directory to scan for .xml, .gz, and .zip files");
    println!("  -c, --config <PATH>      YAML config file path (default: config.yaml)");
    println!("  -h, --help         Show this help message");
}

#[derive(Debug, Clone)]
pub(crate) struct AggregatedStatus {
    pub(crate) org_name: String,
    pub(crate) domain: String,
    pub(crate) pass_count: usize,
    pub(crate) fail_count: usize,
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
            pass_count,
            fail_count,
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
