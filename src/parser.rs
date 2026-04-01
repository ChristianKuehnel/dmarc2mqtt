use crate::imap_client::ReportInput;
use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Serialize;
use std::io::{Cursor, Read};
use zip::ZipArchive;

#[derive(Debug, Serialize)]
pub(crate) struct ScanSummary {
    pub(crate) source: String,
    pub(crate) org_name: Option<String>,
    pub(crate) email: Option<String>,
    pub(crate) policy_published_domain: Option<String>,
    pub(crate) result_pass_count: usize,
    pub(crate) result_fail_count: usize,
}

pub(crate) fn scan_report_inputs(
    inputs: &[ReportInput],
    max_xml_size_mb: u64,
) -> Result<Vec<ScanSummary>, String> {
    let mut summaries = Vec::new();
    let max_xml_bytes = max_xml_size_bytes(max_xml_size_mb)?;

    for input in inputs {
        let extension = input
            .file_name
            .rsplit_once('.')
            .map(|(_, ext)| ext.to_ascii_lowercase());

        match extension.as_deref() {
            Some("xml") => {
                if input.bytes.len() > max_xml_bytes {
                    return Err(format!(
                        "XML attachment {} exceeds configured max_xml_size ({} MB).",
                        input.source, max_xml_size_mb
                    ));
                }
                summaries.push(scan_xml_bytes(&input.bytes, &input.source)?);
            }
            Some("gz") => {
                let decompressed =
                    decompress_gzip_bytes(&input.bytes, &input.source, max_xml_size_mb, max_xml_bytes)?;
                summaries.push(scan_xml_bytes(&decompressed, &input.source)?);
            }
            Some("zip") => {
                let zipped = scan_zip_bytes(&input.bytes, &input.source, max_xml_size_mb, max_xml_bytes)?;
                summaries.extend(zipped);
            }
            _ => {}
        }
    }

    Ok(summaries)
}

fn max_xml_size_bytes(max_xml_size_mb: u64) -> Result<usize, String> {
    let bytes_u64 = max_xml_size_mb
        .checked_mul(1024 * 1024)
        .ok_or_else(|| format!("Config field imap.max_xml_size is too large: {max_xml_size_mb}"))?;
    usize::try_from(bytes_u64)
        .map_err(|_| format!("Config field imap.max_xml_size is too large: {max_xml_size_mb}"))
}

fn gzip_uncompressed_size_hint(input: &[u8]) -> Option<u64> {
    if input.len() < 4 {
        return None;
    }
    let trailer = &input[input.len() - 4..];
    Some(u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]) as u64)
}

fn decompress_gzip_bytes(
    input: &[u8],
    source: &str,
    max_xml_size_mb: u64,
    max_xml_bytes: usize,
) -> Result<Vec<u8>, String> {
    if let Some(size_hint) = gzip_uncompressed_size_hint(input) {
        if size_hint > max_xml_bytes as u64 {
            return Err(format!(
                "GZIP attachment {source} exceeds configured max_xml_size ({} MB).",
                max_xml_size_mb
            ));
        }
    }

    let decoder = flate2::read::GzDecoder::new(input);
    let mut decompressed = Vec::new();
    let limit = max_xml_bytes
        .checked_add(1)
        .ok_or_else(|| format!("Config field imap.max_xml_size is too large: {max_xml_size_mb}"))?;
    decoder
        .take(limit as u64)
        .read_to_end(&mut decompressed)
        .map_err(|err| format!("Failed to decompress gzip attachment {source}: {err}"))?;
    if decompressed.len() > max_xml_bytes {
        return Err(format!(
            "GZIP attachment {source} exceeds configured max_xml_size ({} MB).",
            max_xml_size_mb
        ));
    }
    Ok(decompressed)
}

fn scan_zip_bytes(
    input: &[u8],
    source: &str,
    max_xml_size_mb: u64,
    max_xml_bytes: usize,
) -> Result<Vec<ScanSummary>, String> {
    let cursor = Cursor::new(input);
    let mut archive =
        ZipArchive::new(cursor).map_err(|err| format!("Failed to read ZIP attachment {source}: {err}"))?;
    let mut summaries = Vec::new();

    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|err| format!("Failed to read ZIP entry {index} in {source}: {err}"))?;

        if !entry.is_file() {
            continue;
        }

        let name = entry.name().to_string();
        if !name.to_ascii_lowercase().ends_with(".xml") {
            continue;
        }

        if entry.size() > max_xml_bytes as u64 {
            return Err(format!(
                "ZIP entry {name} in {source} exceeds configured max_xml_size ({} MB).",
                max_xml_size_mb
            ));
        }

        let mut xml_bytes = Vec::new();
        entry
            .take((max_xml_bytes + 1) as u64)
            .read_to_end(&mut xml_bytes)
            .map_err(|err| format!("Failed to read ZIP entry {name} in {source}: {err}"))?;
        if xml_bytes.len() > max_xml_bytes {
            return Err(format!(
                "ZIP entry {name} in {source} exceeds configured max_xml_size ({} MB).",
                max_xml_size_mb
            ));
        }

        let nested_source = format!("{source}:{name}");
        summaries.push(scan_xml_bytes(&xml_bytes, &nested_source)?);
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
