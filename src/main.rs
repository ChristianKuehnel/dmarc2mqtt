use quick_xml::events::Event;
use quick_xml::Reader;
use std::env;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

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
    let xml_path = parse_xml_arg()?;
    let summary = scan_xml(&xml_path)?;

    println!("Scanned XML file: {}", xml_path);
    println!("Root element: {}", summary.root_element);
    println!("Element count: {}", summary.element_count);
    println!("Attribute count: {}", summary.attribute_count);

    Ok(())
}

fn parse_xml_arg() -> Result<String, String> {
    let mut args = env::args().skip(1);
    let mut xml_path: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--xml" | "-x" => {
                let value = args
                    .next()
                    .ok_or_else(|| "Missing value for --xml. Usage: dmarc2mqtt --xml <PATH>".to_owned())?;
                xml_path = Some(value);
            }
            "--help" | "-h" => {
                print_help();
                return Err(String::new());
            }
            _ => {
                return Err(format!(
                    "Unknown argument: {arg}\nUsage: dmarc2mqtt --xml <PATH>"
                ));
            }
        }
    }

    xml_path.ok_or_else(|| "Missing required argument: --xml <PATH>".to_owned())
}

fn print_help() {
    println!("Usage: dmarc2mqtt --xml <PATH>");
    println!();
    println!("Options:");
    println!("  -x, --xml <PATH>   XML file to scan");
    println!("  -h, --help         Show this help message");
}

struct ScanSummary {
    root_element: String,
    element_count: usize,
    attribute_count: usize,
}

fn scan_xml(path: &str) -> Result<ScanSummary, String> {
    if !Path::new(path).exists() {
        return Err(format!("File not found: {path}"));
    }

    let input = fs::read_to_string(path).map_err(|err| format!("Failed to read {path}: {err}"))?;
    let mut reader = Reader::from_str(&input);
    reader.config_mut().trim_text(true);

    let mut element_count = 0usize;
    let mut attribute_count = 0usize;
    let mut root_element: Option<String> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                element_count += 1;
                attribute_count += e.attributes().filter_map(Result::ok).count();
                if root_element.is_none() {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    root_element = Some(name);
                }
            }
            Ok(Event::Empty(e)) => {
                element_count += 1;
                attribute_count += e.attributes().filter_map(Result::ok).count();
                if root_element.is_none() {
                    let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                    root_element = Some(name);
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(err) => {
                return Err(format!("XML parse error in {path}: {err}"));
            }
        }
    }

    let root_element = root_element.unwrap_or_else(|| "<empty document>".to_owned());
    Ok(ScanSummary {
        root_element,
        element_count,
        attribute_count,
    })
}
