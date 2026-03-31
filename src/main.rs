use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::env;
use std::fs;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use walkdir::WalkDir;
use zip::ZipArchive;

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
    let config = load_config(&args.config_path)?;
    let summaries = scan_directory(&args.directory_path)?;

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

    print_aggregated_status(&summaries);

    publish_reports_to_mqtt(&config, &summaries)?;

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

#[derive(Debug, Deserialize)]
struct AppConfig {
    mqtt: MqttConfig,
}

#[derive(Debug, Deserialize)]
struct MqttConfig {
    server_name: String,
    server_port: u16,
    login: String,
    password: String,
    base_topic: String,
}

fn load_config(path: &str) -> Result<AppConfig, String> {
    let content = fs::read_to_string(path)
        .map_err(|err| format!("Failed to read config file {path}: {err}"))?;
    let config: AppConfig =
        serde_yaml::from_str(&content).map_err(|err| format!("Invalid YAML in {path}: {err}"))?;

    if config.mqtt.server_name.trim().is_empty() {
        return Err("Config field mqtt.server_name must not be empty".to_owned());
    }
    if config.mqtt.login.trim().is_empty() {
        return Err("Config field mqtt.login must not be empty".to_owned());
    }
    if config.mqtt.password.trim().is_empty() {
        return Err("Config field mqtt.password must not be empty".to_owned());
    }
    if config.mqtt.base_topic.trim().is_empty() {
        return Err("Config field mqtt.base_topic must not be empty".to_owned());
    }
    if config.mqtt.server_port == 0 {
        return Err("Config field mqtt.server_port must be greater than 0".to_owned());
    }

    Ok(config)
}

#[derive(Debug, Serialize)]
struct ScanSummary {
    source: String,
    org_name: Option<String>,
    email: Option<String>,
    policy_published_domain: Option<String>,
    result_pass_count: usize,
    result_fail_count: usize,
}

#[derive(Serialize)]
struct PublishSummary {
    file_count: usize,
}

fn scan_directory(path: &str) -> Result<Vec<ScanSummary>, String> {
    let mut summaries = Vec::new();

    for entry_result in WalkDir::new(path) {
        let entry = entry_result.map_err(|err| format!("Failed to walk directory {path}: {err}"))?;
        if !entry.file_type().is_file() {
            continue;
        }

        let file_path = entry.path();
        let extension = file_path
            .extension()
            .and_then(OsStr::to_str)
            .map(|e| e.to_ascii_lowercase());

        match extension.as_deref() {
            Some("xml") => {
                let input = fs::read(file_path).map_err(|err| {
                    format!("Failed to read {}: {err}", file_path.display())
                })?;
                summaries.push(scan_xml_bytes(&input, &file_path.display().to_string())?);
            }
            Some("gz") => {
                let input = read_gzip_file(file_path)?;
                summaries.push(scan_xml_bytes(&input, &file_path.display().to_string())?);
            }
            Some("zip") => {
                let zip_summaries = scan_zip_file(file_path)?;
                summaries.extend(zip_summaries);
            }
            _ => {}
        }
    }

    Ok(summaries)
}

fn read_gzip_file(path: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|err| format!("Failed to open {}: {err}", path.display()))?;
    let mut decoder = flate2::read::GzDecoder::new(file);
    let mut decompressed = Vec::new();
    decoder
        .read_to_end(&mut decompressed)
        .map_err(|err| format!("Failed to decompress {}: {err}", path.display()))?;
    Ok(decompressed)
}

fn scan_zip_file(path: &Path) -> Result<Vec<ScanSummary>, String> {
    let file = File::open(path).map_err(|err| format!("Failed to open {}: {err}", path.display()))?;
    let mut archive =
        ZipArchive::new(file).map_err(|err| format!("Failed to open ZIP {}: {err}", path.display()))?;
    let mut summaries = Vec::new();

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|err| {
            format!("Failed to read entry {index} in {}: {err}", path.display())
        })?;

        if !entry.is_file() {
            continue;
        }

        let name = entry.name().to_string();
        if !name.to_ascii_lowercase().ends_with(".xml") {
            continue;
        }

        let mut input = Vec::new();
        entry.read_to_end(&mut input).map_err(|err| {
            format!(
                "Failed to decompress entry {name} in {}: {err}",
                path.display()
            )
        })?;

        let source = format!("{}:{name}", path.display());
        summaries.push(scan_xml_bytes(&input, &source)?);
    }

    Ok(summaries)
}

fn scan_xml_bytes(input: &[u8], source: &str) -> Result<ScanSummary, String> {
    let mut reader = Reader::from_reader(input);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut org_name: Option<String> = None;
    let mut email: Option<String> = None;
    let mut policy_published_domain: Option<String> = None;
    let mut result_pass_count = 0usize;
    let mut result_fail_count = 0usize;
    let mut stack: Vec<String> = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = local_name(e.name().as_ref());
                stack.push(name);
            }
            Ok(Event::Empty(e)) => {
                let _ = local_name(e.name().as_ref());
            }
            Ok(Event::Text(e)) => {
                let text = e
                    .xml10_content()
                    .map_err(|err| format!("Failed to decode text in {source}: {err}"))?
                    .trim()
                    .to_string();
                if text.is_empty() {
                    buf.clear();
                    continue;
                }

                let current = stack.last().map(String::as_str);
                match current {
                    Some("org_name") if org_name.is_none() => org_name = Some(text),
                    Some("email") if email.is_none() => email = Some(text),
                    Some("result") => match text.to_ascii_lowercase().as_str() {
                        "pass" => result_pass_count += 1,
                        "fail" => result_fail_count += 1,
                        _ => {}
                    },
                    Some("domain")
                        if policy_published_domain.is_none()
                            && stack.len() >= 2
                            && stack[stack.len() - 2] == "policy_published" =>
                    {
                        policy_published_domain = Some(text)
                    }
                    _ => {}
                }
            }
            Ok(Event::End(_)) => {
                stack.pop();
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(err) => {
                return Err(format!("XML parse error in {source}: {err}"));
            }
        }
        buf.clear();
    }

    Ok(ScanSummary {
        source: source.to_owned(),
        org_name,
        email,
        policy_published_domain,
        result_pass_count,
        result_fail_count,
    })
}

fn local_name(name: &[u8]) -> String {
    let raw = String::from_utf8_lossy(name);
    match raw.rsplit_once(':') {
        Some((_, local)) => local.to_string(),
        None => raw.to_string(),
    }
}

fn print_aggregated_status(summaries: &[ScanSummary]) {
    let mut grouped: HashMap<(String, String), (usize, usize)> = HashMap::new();

    for summary in summaries {
        let org_name = summary
            .org_name
            .as_deref()
            .unwrap_or("<missing>")
            .to_owned();
        let domain = summary
            .policy_published_domain
            .as_deref()
            .unwrap_or("<missing>")
            .to_owned();

        let entry = grouped.entry((org_name, domain)).or_insert((0, 0));
        entry.0 += summary.result_pass_count;
        entry.1 += summary.result_fail_count;
    }

    println!("Aggregated status:");
    for ((org_name, domain), (pass_count, fail_count)) in grouped {
        let total = pass_count + fail_count;
        let status = if fail_count == 0 {
            "pass".to_owned()
        } else {
            let percent_failed = (fail_count as f64 / total as f64) * 100.0;
            format!("{percent_failed:.1}% failed")
        };

        println!("  {org_name} / {domain}: {status}");
    }
}

fn publish_reports_to_mqtt(config: &AppConfig, summaries: &[ScanSummary]) -> Result<(), String> {
    let mut mqtt_options = rumqttc::MqttOptions::new(
        "dmarc2mqtt",
        config.mqtt.server_name.clone(),
        config.mqtt.server_port,
    );
    mqtt_options.set_credentials(config.mqtt.login.clone(), config.mqtt.password.clone());
    mqtt_options.set_keep_alive(Duration::from_secs(10));

    let (client, mut connection) = rumqttc::Client::new(mqtt_options, 20);
    let running = Arc::new(AtomicBool::new(true));
    let connection_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let running_bg = Arc::clone(&running);
    let connection_error_bg = Arc::clone(&connection_error);

    let network_thread = thread::spawn(move || {
        for notification in connection.iter() {
            if !running_bg.load(Ordering::Relaxed) {
                break;
            }
            if let Err(err) = notification {
                if let Ok(mut slot) = connection_error_bg.lock() {
                    *slot = Some(err.to_string());
                }
                break;
            }
        }
    });

    let reports_topic = format!("{}/reports", config.mqtt.base_topic);
    let summary_topic = format!("{}/summary", config.mqtt.base_topic);

    for report in summaries {
        let payload = serde_json::to_vec(report)
            .map_err(|err| format!("Failed to serialize MQTT report payload: {err}"))?;
        client
            .publish(
                reports_topic.clone(),
                rumqttc::QoS::AtLeastOnce,
                false,
                payload,
            )
            .map_err(|err| format!("Failed to publish report to MQTT: {err}"))?;
    }

    let summary_payload = serde_json::to_vec(&PublishSummary {
        file_count: summaries.len(),
    })
    .map_err(|err| format!("Failed to serialize MQTT summary payload: {err}"))?;
    client
        .publish(
            summary_topic,
            rumqttc::QoS::AtLeastOnce,
            false,
            summary_payload,
        )
        .map_err(|err| format!("Failed to publish summary to MQTT: {err}"))?;

    thread::sleep(Duration::from_millis(300));
    let _ = client.disconnect();
    running.store(false, Ordering::Relaxed);
    let _ = network_thread.join();

    if let Ok(slot) = connection_error.lock() {
        if let Some(err) = &*slot {
            return Err(format!("MQTT connection error: {err}"));
        }
    }

    Ok(())
}
