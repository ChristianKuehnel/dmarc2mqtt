use quick_xml::events::Event;
use quick_xml::Reader;
use std::ffi::OsStr;
use std::env;
use std::fs;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::ExitCode;
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
    let input_dir = parse_directory_arg()?;
    let summaries = scan_directory(&input_dir)?;

    if summaries.is_empty() {
        return Err(format!(
            "No supported files found in {input_dir}. Expected .xml, .gz, or .zip."
        ));
    }

    let total_elements: usize = summaries.iter().map(|s| s.element_count).sum();
    let total_attributes: usize = summaries.iter().map(|s| s.attribute_count).sum();

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
        println!("  Root element: {}", summary.root_element);
        println!("  Element count: {}", summary.element_count);
        println!("  Attribute count: {}", summary.attribute_count);
    }

    println!("Total documents: {}", summaries.len());
    println!("Total element count: {total_elements}");
    println!("Total attribute count: {total_attributes}");

    Ok(())
}

fn parse_directory_arg() -> Result<String, String> {
    let mut args = env::args().skip(1);
    let mut directory_path: Option<String> = None;

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
            "--help" | "-h" => {
                print_help();
                return Err(String::new());
            }
            _ => {
                return Err(format!(
                    "Unknown argument: {arg}\nUsage: dmarc2mqtt --directory <PATH>"
                ));
            }
        }
    }

    let directory_path =
        directory_path.ok_or_else(|| "Missing required argument: --directory <PATH>".to_owned())?;

    if !Path::new(&directory_path).is_dir() {
        return Err(format!("Not a directory: {directory_path}"));
    }

    Ok(directory_path)
}

fn print_help() {
    println!("Usage: dmarc2mqtt --directory <PATH>");
    println!();
    println!("Options:");
    println!("  -d, --directory <PATH>   Directory to scan for .xml, .gz, and .zip files");
    println!("  -h, --help         Show this help message");
}

struct ScanSummary {
    source: String,
    org_name: Option<String>,
    email: Option<String>,
    policy_published_domain: Option<String>,
    root_element: String,
    element_count: usize,
    attribute_count: usize,
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
    let mut element_count = 0usize;
    let mut attribute_count = 0usize;
    let mut root_element: Option<String> = None;
    let mut org_name: Option<String> = None;
    let mut email: Option<String> = None;
    let mut policy_published_domain: Option<String> = None;
    let mut stack: Vec<String> = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = local_name(e.name().as_ref());
                element_count += 1;
                attribute_count += e.attributes().filter_map(Result::ok).count();
                if root_element.is_none() {
                    root_element = Some(name.clone());
                }
                stack.push(name);
            }
            Ok(Event::Empty(e)) => {
                let name = local_name(e.name().as_ref());
                element_count += 1;
                attribute_count += e.attributes().filter_map(Result::ok).count();
                if root_element.is_none() {
                    root_element = Some(name);
                }
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

    let root_element = root_element.unwrap_or_else(|| "<empty document>".to_owned());
    Ok(ScanSummary {
        source: source.to_owned(),
        org_name,
        email,
        policy_published_domain,
        root_element,
        element_count,
        attribute_count,
    })
}

fn local_name(name: &[u8]) -> String {
    let raw = String::from_utf8_lossy(name);
    match raw.rsplit_once(':') {
        Some((_, local)) => local.to_string(),
        None => raw.to_string(),
    }
}
