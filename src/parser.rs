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

#[derive(Debug, Clone, Copy)]
pub(crate) struct ZipLimits {
    pub(crate) max_entries: usize,
    pub(crate) max_xml_files: usize,
    pub(crate) max_uncompressed_size_mb: Option<u64>,
}

pub(crate) fn scan_report_inputs(
    inputs: &[ReportInput],
    max_xml_size_mb: u64,
    zip_limits: ZipLimits,
) -> Result<Vec<ScanSummary>, String> {
    let mut summaries = Vec::new();
    let max_xml_bytes = max_xml_size_bytes(max_xml_size_mb)?;
    let max_zip_uncompressed_bytes =
        max_zip_uncompressed_bytes(zip_limits.max_uncompressed_size_mb, max_xml_size_mb)?;

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
                let summary = scan_xml_bytes(&input.bytes, &input.source)?;
                validate_report_sender(&summary, &input.message_sender)?;
                summaries.push(summary);
            }
            Some("gz") => {
                let decompressed = decompress_gzip_bytes(
                    &input.bytes,
                    &input.source,
                    max_xml_size_mb,
                    max_xml_bytes,
                )?;
                let summary = scan_xml_bytes(&decompressed, &input.source)?;
                validate_report_sender(&summary, &input.message_sender)?;
                summaries.push(summary);
            }
            Some("zip") => {
                let zipped = scan_zip_bytes(
                    &input.bytes,
                    &input.source,
                    max_xml_size_mb,
                    max_xml_bytes,
                    zip_limits,
                    max_zip_uncompressed_bytes,
                )?;
                for summary in &zipped {
                    validate_report_sender(summary, &input.message_sender)?;
                }
                summaries.extend(zipped);
            }
            _ => {}
        }
    }

    Ok(summaries)
}

fn max_zip_uncompressed_bytes(
    max_zip_uncompressed_size_mb: Option<u64>,
    max_xml_size_mb: u64,
) -> Result<usize, String> {
    let size_mb = max_zip_uncompressed_size_mb.unwrap_or(max_xml_size_mb);
    let bytes_u64 = size_mb
        .checked_mul(1024 * 1024)
        .ok_or_else(|| format!("Configured max_zip_uncompressed_size is too large: {size_mb}"))?;
    usize::try_from(bytes_u64)
        .map_err(|_| format!("Configured max_zip_uncompressed_size is too large: {size_mb}"))
}

fn validate_report_sender(summary: &ScanSummary, message_sender: &str) -> Result<(), String> {
    let report_email = summary.email.as_deref().ok_or_else(|| {
        format!(
            "DMARC report {} is missing report_metadata/email and cannot be authenticated against message sender {}.",
            summary.source, message_sender
        )
    })?;

    if report_email
        .trim()
        .eq_ignore_ascii_case(message_sender.trim())
    {
        Ok(())
    } else {
        Err(format!(
            "DMARC report {} claims report_metadata/email '{}' but message sender is '{}'.",
            summary.source, report_email, message_sender
        ))
    }
}

fn max_xml_size_bytes(max_xml_size_mb: u64) -> Result<usize, String> {
    let bytes_u64 = max_xml_size_mb
        .checked_mul(1024 * 1024)
        .ok_or_else(|| format!("Configured max_xml_size is too large: {max_xml_size_mb}"))?;
    usize::try_from(bytes_u64)
        .map_err(|_| format!("Configured max_xml_size is too large: {max_xml_size_mb}"))
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
        .ok_or_else(|| format!("Configured max_xml_size is too large: {max_xml_size_mb}"))?;
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
    zip_limits: ZipLimits,
    max_zip_uncompressed_bytes: usize,
) -> Result<Vec<ScanSummary>, String> {
    let cursor = Cursor::new(input);
    let mut archive = ZipArchive::new(cursor)
        .map_err(|err| format!("Failed to read ZIP attachment {source}: {err}"))?;
    if archive.len() > zip_limits.max_entries {
        return Err(format!(
            "ZIP attachment {source} contains {} entries, exceeding max_zip_entries ({}).",
            archive.len(),
            zip_limits.max_entries
        ));
    }

    let mut summaries = Vec::new();
    let mut xml_file_count = 0usize;
    let mut total_uncompressed_xml_bytes = 0usize;

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
        xml_file_count += 1;
        if xml_file_count > zip_limits.max_xml_files {
            return Err(format!(
                "ZIP attachment {source} contains more than {} XML file(s).",
                zip_limits.max_xml_files
            ));
        }

        if entry.size() > max_xml_bytes as u64 {
            return Err(format!(
                "ZIP entry {name} in {source} exceeds configured max_xml_size ({} MB).",
                max_xml_size_mb
            ));
        }
        total_uncompressed_xml_bytes = total_uncompressed_xml_bytes
            .checked_add(entry.size() as usize)
            .ok_or_else(|| {
                format!("ZIP attachment {source} aggregate uncompressed XML size is too large.")
            })?;
        if total_uncompressed_xml_bytes > max_zip_uncompressed_bytes {
            return Err(format!(
                "ZIP attachment {source} aggregate uncompressed XML size exceeds max_zip_uncompressed_size."
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct ExpectedSummary {
        file_name: &'static str,
        org_name: &'static str,
        email: &'static str,
        pass_count: usize,
        fail_count: usize,
    }

    fn default_zip_limits() -> ZipLimits {
        ZipLimits {
            max_entries: 1000,
            max_xml_files: 10,
            max_uncompressed_size_mb: None,
        }
    }

    #[test]
    fn scans_synthetic_dmarc_xml_fixtures() {
        let fixtures = [
            (
                include_bytes!("../tests/fixtures/dmarc-yahoo.xml.gz").as_slice(),
                ExpectedSummary {
                    file_name: "dmarc-yahoo.xml.gz",
                    org_name: "Yahoo",
                    email: "dmarchelp@yahooinc.com",
                    pass_count: 2,
                    fail_count: 0,
                },
            ),
            (
                include_bytes!("../tests/fixtures/dmarc-gmx.xml").as_slice(),
                ExpectedSummary {
                    file_name: "dmarc-gmx.xml",
                    org_name: "GMX",
                    email: "noreply-dmarc@sicher.gmx.net",
                    pass_count: 2,
                    fail_count: 0,
                },
            ),
            (
                include_bytes!("../tests/fixtures/dmarc-outlook.xml").as_slice(),
                ExpectedSummary {
                    file_name: "dmarc-outlook.xml",
                    org_name: "Enterprise Outlook",
                    email: "dmarcreport@microsoft.com",
                    pass_count: 6,
                    fail_count: 0,
                },
            ),
            (
                include_bytes!("../tests/fixtures/dmarc-google.zip").as_slice(),
                ExpectedSummary {
                    file_name: "dmarc-google.zip",
                    org_name: "google.com",
                    email: "noreply-dmarc-support@google.com",
                    pass_count: 16,
                    fail_count: 0,
                },
            ),
        ];

        for (bytes, expected) in fixtures {
            let input = ReportInput {
                source: format!("fixture:{}", expected.file_name),
                file_name: expected.file_name.to_owned(),
                message_sender: expected.email.to_owned(),
                bytes: bytes.to_vec(),
            };

            let summaries = scan_report_inputs(&[input], 1, default_zip_limits())
                .expect("fixture should parse");

            assert_eq!(summaries.len(), 1);
            let summary = &summaries[0];
            assert_eq!(summary.org_name.as_deref(), Some(expected.org_name));
            assert_eq!(summary.email.as_deref(), Some(expected.email));
            assert_eq!(
                summary.policy_published_domain.as_deref(),
                Some("example.com")
            );
            assert_eq!(summary.result_pass_count, expected.pass_count);
            assert_eq!(summary.result_fail_count, expected.fail_count);
        }
    }

    #[test]
    fn rejects_report_email_that_does_not_match_message_sender() {
        let input = ReportInput {
            source: "fixture:dmarc-gmx.xml".to_owned(),
            file_name: "dmarc-gmx.xml".to_owned(),
            message_sender: "attacker@example.com".to_owned(),
            bytes: include_bytes!("../tests/fixtures/dmarc-gmx.xml").to_vec(),
        };

        let err = scan_report_inputs(&[input], 1, default_zip_limits())
            .expect_err("sender mismatch should fail");

        assert!(err.contains("claims report_metadata/email"));
        assert!(err.contains("attacker@example.com"));
    }

    #[test]
    fn rejects_zip_with_too_many_entries() {
        let input = zip_input(&[("one.txt", b"ignored".as_slice()), ("two.txt", b"ignored")]);

        let err = scan_report_inputs(
            &[input],
            1,
            ZipLimits {
                max_entries: 1,
                ..default_zip_limits()
            },
        )
        .expect_err("zip entry limit should fail");

        assert!(err.contains("max_zip_entries"));
    }

    #[test]
    fn rejects_zip_with_too_many_xml_files() {
        let input = zip_input(&[
            ("one.xml", dmarc_xml("noreply@example.com").as_bytes()),
            ("two.xml", dmarc_xml("noreply@example.com").as_bytes()),
        ]);

        let err = scan_report_inputs(
            &[input],
            1,
            ZipLimits {
                max_xml_files: 1,
                ..default_zip_limits()
            },
        )
        .expect_err("zip xml file limit should fail");

        assert!(err.contains("more than 1 XML file"));
    }

    #[test]
    fn rejects_zip_exceeding_aggregate_uncompressed_limit() {
        let input = zip_input(&[
            ("one.xml", dmarc_xml("noreply@example.com").as_bytes()),
            ("two.xml", dmarc_xml("noreply@example.com").as_bytes()),
        ]);

        let err = scan_zip_bytes(
            &input.bytes,
            &input.source,
            1,
            1024 * 1024,
            default_zip_limits(),
            32,
        )
        .expect_err("zip aggregate size limit should fail");

        assert!(err.contains("aggregate uncompressed XML size"));
    }

    fn zip_input(entries: &[(&str, &[u8])]) -> ReportInput {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default();
            for (name, bytes) in entries {
                writer
                    .start_file(*name, options)
                    .expect("zip entry should start");
                writer
                    .write_all(bytes)
                    .expect("zip entry should be written");
            }
            writer.finish().expect("zip should finish");
        }

        ReportInput {
            source: "fixture:reports.zip".to_owned(),
            file_name: "reports.zip".to_owned(),
            message_sender: "noreply@example.com".to_owned(),
            bytes: cursor.into_inner(),
        }
    }

    fn dmarc_xml(email: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<feedback>
  <report_metadata>
    <org_name>Example</org_name>
    <email>{email}</email>
  </report_metadata>
  <policy_published>
    <domain>example.com</domain>
  </policy_published>
  <record>
    <row>
      <policy_evaluated>
        <dkim>pass</dkim>
        <spf>pass</spf>
      </policy_evaluated>
    </row>
  </record>
</feedback>"#
        )
    }
}
