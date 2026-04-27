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
    validate_dmarc_xml_schema(input, source)?;

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
        policy_published_domain: match policy_published_domain {
            Some(domain) => Some(normalize_report_domain(&domain, source)?),
            None => None,
        },
        result_pass_count,
        result_fail_count,
    })
}

fn normalize_report_domain(domain: &str, source: &str) -> Result<String, String> {
    let normalized = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    validate_report_domain(&normalized)
        .map_err(|err| format!("Invalid policy_published/domain in {source}: {err}"))?;
    Ok(normalized)
}

fn validate_report_domain(domain: &str) -> Result<(), &'static str> {
    if domain.is_empty() {
        return Err("domain must not be empty");
    }
    if domain.len() > 253 {
        return Err("domain is longer than 253 characters");
    }
    if !domain.contains('.') {
        return Err("domain must contain at least one dot");
    }
    for label in domain.split('.') {
        if label.is_empty() {
            return Err("domain contains an empty label");
        }
        if label.len() > 63 {
            return Err("domain label is longer than 63 characters");
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err("domain labels must not start or end with hyphen");
        }
        if !label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err("domain contains characters outside ASCII letters, digits, and hyphen");
        }
    }
    Ok(())
}

#[derive(Debug)]
struct DmarcElement {
    name: String,
    text: String,
    children: Vec<DmarcElement>,
}

#[derive(Clone, Copy)]
struct ChildSpec {
    name: &'static str,
    min: usize,
    max: Option<usize>,
}

impl ChildSpec {
    const fn required(name: &'static str) -> Self {
        Self {
            name,
            min: 1,
            max: Some(1),
        }
    }

    const fn optional(name: &'static str) -> Self {
        Self {
            name,
            min: 0,
            max: Some(1),
        }
    }

    const fn unbounded(name: &'static str, min: usize) -> Self {
        Self {
            name,
            min,
            max: None,
        }
    }
}

fn validate_dmarc_xml_schema(input: &[u8], source: &str) -> Result<(), String> {
    let root = parse_xml_tree(input, source)?;
    validate_feedback(&root)
        .map_err(|err| format!("XML schema validation failed in {source}: {err}"))
}

fn parse_xml_tree(input: &[u8], source: &str) -> Result<DmarcElement, String> {
    let mut reader = Reader::from_reader(input);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut stack: Vec<DmarcElement> = Vec::new();
    let mut root: Option<DmarcElement> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                stack.push(DmarcElement {
                    name: local_name(e.name().as_ref()),
                    text: String::new(),
                    children: Vec::new(),
                });
            }
            Ok(Event::Empty(e)) => {
                let element = DmarcElement {
                    name: local_name(e.name().as_ref()),
                    text: String::new(),
                    children: Vec::new(),
                };
                attach_xml_element(element, &mut stack, &mut root, source)?;
            }
            Ok(Event::Text(e)) => {
                let text = e
                    .xml10_content()
                    .map_err(|err| format!("Failed to decode text in {source}: {err}"))?;
                if let Some(current) = stack.last_mut() {
                    current.text.push_str(text.trim());
                } else if !text.trim().is_empty() {
                    return Err(format!("Unexpected text outside root element in {source}"));
                }
            }
            Ok(Event::End(e)) => {
                let end_name = local_name(e.name().as_ref());
                let element = stack
                    .pop()
                    .ok_or_else(|| format!("Unexpected closing tag </{end_name}> in {source}"))?;
                if element.name != end_name {
                    return Err(format!(
                        "Mismatched XML tag in {source}: opened <{}> but closed </{}>",
                        element.name, end_name
                    ));
                }
                attach_xml_element(element, &mut stack, &mut root, source)?;
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(err) => return Err(format!("XML parse error in {source}: {err}")),
        }
        buf.clear();
    }

    if let Some(open) = stack.last() {
        return Err(format!("Unclosed XML tag <{}> in {source}", open.name));
    }
    root.ok_or_else(|| format!("XML attachment {source} does not contain a root element"))
}

fn attach_xml_element(
    element: DmarcElement,
    stack: &mut [DmarcElement],
    root: &mut Option<DmarcElement>,
    source: &str,
) -> Result<(), String> {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(element);
    } else if root.is_none() {
        *root = Some(element);
    } else {
        return Err(format!(
            "XML attachment {source} contains multiple root elements"
        ));
    }
    Ok(())
}

fn validate_feedback(element: &DmarcElement) -> Result<(), String> {
    if element.name != "feedback" {
        return Err(format!(
            "root element must be <feedback>, found <{}>",
            element.name
        ));
    }
    expect_sequence(
        element,
        &[
            ChildSpec::optional("version"),
            ChildSpec::required("report_metadata"),
            ChildSpec::required("policy_published"),
            ChildSpec::unbounded("record", 1),
        ],
    )?;
    if let Some(version) = child(element, "version") {
        expect_decimal(version)?;
    }
    validate_report_metadata(require_child(element, "report_metadata")?)?;
    validate_policy_published(require_child(element, "policy_published")?)?;
    for record in children(element, "record") {
        validate_record(record)?;
    }
    Ok(())
}

fn validate_report_metadata(element: &DmarcElement) -> Result<(), String> {
    expect_sequence(
        element,
        &[
            ChildSpec::required("org_name"),
            ChildSpec::required("email"),
            ChildSpec::optional("extra_contact_info"),
            ChildSpec::required("report_id"),
            ChildSpec::required("date_range"),
            ChildSpec::unbounded("error", 0),
        ],
    )?;
    expect_text_child(element, "org_name")?;
    expect_text_child(element, "email")?;
    expect_text_child(element, "report_id")?;
    validate_date_range(require_child(element, "date_range")?)?;
    Ok(())
}

fn validate_date_range(element: &DmarcElement) -> Result<(), String> {
    expect_all(
        element,
        &[ChildSpec::required("begin"), ChildSpec::required("end")],
    )?;
    expect_integer(require_child(element, "begin")?)?;
    expect_integer(require_child(element, "end")?)?;
    Ok(())
}

fn validate_policy_published(element: &DmarcElement) -> Result<(), String> {
    expect_all(
        element,
        &[
            ChildSpec::required("domain"),
            ChildSpec::optional("adkim"),
            ChildSpec::optional("aspf"),
            ChildSpec::required("p"),
            ChildSpec::optional("sp"),
            ChildSpec::optional("pct"),
            ChildSpec::optional("fo"),
            ChildSpec::optional("np"),
            ChildSpec::optional("discovery_method"),
            ChildSpec::optional("testing"),
        ],
    )?;
    expect_text_child(element, "domain")?;
    if let Some(adkim) = child(element, "adkim") {
        expect_enum(adkim, &["r", "s"])?;
    }
    if let Some(aspf) = child(element, "aspf") {
        expect_enum(aspf, &["r", "s"])?;
    }
    expect_enum(
        require_child(element, "p")?,
        &["none", "quarantine", "reject"],
    )?;
    if let Some(sp) = child(element, "sp") {
        expect_enum(sp, &["none", "quarantine", "reject"])?;
    }
    if let Some(pct) = child(element, "pct") {
        expect_integer(pct)?;
    }
    Ok(())
}

fn validate_record(element: &DmarcElement) -> Result<(), String> {
    expect_sequence(
        element,
        &[
            ChildSpec::required("row"),
            ChildSpec::required("identifiers"),
            ChildSpec::required("auth_results"),
        ],
    )?;
    validate_row(require_child(element, "row")?)?;
    validate_identifiers(require_child(element, "identifiers")?)?;
    validate_auth_results(require_child(element, "auth_results")?)?;
    Ok(())
}

fn validate_row(element: &DmarcElement) -> Result<(), String> {
    expect_all(
        element,
        &[
            ChildSpec::required("source_ip"),
            ChildSpec::required("count"),
            ChildSpec::required("policy_evaluated"),
        ],
    )?;
    expect_text_child(element, "source_ip")?;
    expect_integer(require_child(element, "count")?)?;
    validate_policy_evaluated(require_child(element, "policy_evaluated")?)?;
    Ok(())
}

fn validate_policy_evaluated(element: &DmarcElement) -> Result<(), String> {
    expect_sequence(
        element,
        &[
            ChildSpec::required("disposition"),
            ChildSpec::required("dkim"),
            ChildSpec::required("spf"),
            ChildSpec::unbounded("reason", 0),
        ],
    )?;
    expect_enum(
        require_child(element, "disposition")?,
        &["none", "quarantine", "reject"],
    )?;
    expect_enum(require_child(element, "dkim")?, &["pass", "fail"])?;
    expect_enum(require_child(element, "spf")?, &["pass", "fail"])?;
    for reason in children(element, "reason") {
        validate_policy_override_reason(reason)?;
    }
    Ok(())
}

fn validate_policy_override_reason(element: &DmarcElement) -> Result<(), String> {
    expect_all(
        element,
        &[ChildSpec::required("type"), ChildSpec::optional("comment")],
    )?;
    expect_enum(
        require_child(element, "type")?,
        &[
            "forwarded",
            "sampled_out",
            "trusted_forwarder",
            "mailing_list",
            "local_policy",
            "other",
        ],
    )
}

fn validate_identifiers(element: &DmarcElement) -> Result<(), String> {
    expect_all(
        element,
        &[
            ChildSpec::optional("envelope_to"),
            ChildSpec::optional("envelope_from"),
            ChildSpec::required("header_from"),
        ],
    )?;
    expect_text_child(element, "header_from")?;
    Ok(())
}

fn validate_auth_results(element: &DmarcElement) -> Result<(), String> {
    expect_sequence(
        element,
        &[
            ChildSpec::unbounded("dkim", 0),
            ChildSpec::unbounded("spf", 1),
        ],
    )?;
    for dkim in children(element, "dkim") {
        validate_dkim_auth_result(dkim)?;
    }
    for spf in children(element, "spf") {
        validate_spf_auth_result(spf)?;
    }
    Ok(())
}

fn validate_dkim_auth_result(element: &DmarcElement) -> Result<(), String> {
    expect_all(
        element,
        &[
            ChildSpec::required("domain"),
            ChildSpec::optional("selector"),
            ChildSpec::required("result"),
            ChildSpec::optional("human_result"),
        ],
    )?;
    expect_text_child(element, "domain")?;
    expect_enum(
        require_child(element, "result")?,
        &[
            "none",
            "pass",
            "fail",
            "policy",
            "neutral",
            "temperror",
            "permerror",
        ],
    )
}

fn validate_spf_auth_result(element: &DmarcElement) -> Result<(), String> {
    expect_all(
        element,
        &[
            ChildSpec::required("domain"),
            ChildSpec::optional("scope"),
            ChildSpec::required("result"),
        ],
    )?;
    expect_text_child(element, "domain")?;
    if let Some(scope) = child(element, "scope") {
        expect_enum(scope, &["helo", "mfrom"])?;
    }
    expect_enum(
        require_child(element, "result")?,
        &[
            "none",
            "neutral",
            "pass",
            "fail",
            "softfail",
            "temperror",
            "permerror",
        ],
    )
}

fn expect_sequence(element: &DmarcElement, specs: &[ChildSpec]) -> Result<(), String> {
    let mut index = 0usize;
    for spec in specs {
        let mut count = 0usize;
        while index < element.children.len() && element.children[index].name == spec.name {
            count += 1;
            index += 1;
            if spec.max.is_some_and(|max| count > max) {
                return Err(format!(
                    "<{}> contains too many <{}> elements",
                    element.name, spec.name
                ));
            }
        }
        if count < spec.min {
            return Err(format!(
                "<{}> is missing required <{}> element",
                element.name, spec.name
            ));
        }
    }
    if let Some(extra) = element.children.get(index) {
        return Err(format!(
            "<{}> contains unexpected <{}> element",
            element.name, extra.name
        ));
    }
    Ok(())
}

fn expect_all(element: &DmarcElement, specs: &[ChildSpec]) -> Result<(), String> {
    for child in &element.children {
        let Some(spec) = specs.iter().find(|spec| spec.name == child.name) else {
            return Err(format!(
                "<{}> contains unexpected <{}> element",
                element.name, child.name
            ));
        };
        let count = element
            .children
            .iter()
            .filter(|candidate| candidate.name == child.name)
            .count();
        if spec.max.is_some_and(|max| count > max) {
            return Err(format!(
                "<{}> contains too many <{}> elements",
                element.name, child.name
            ));
        }
    }
    for spec in specs {
        let count = element
            .children
            .iter()
            .filter(|candidate| candidate.name == spec.name)
            .count();
        if count < spec.min {
            return Err(format!(
                "<{}> is missing required <{}> element",
                element.name, spec.name
            ));
        }
    }
    Ok(())
}

fn expect_text_child(element: &DmarcElement, name: &str) -> Result<(), String> {
    expect_text(require_child(element, name)?)
}

fn expect_text(element: &DmarcElement) -> Result<(), String> {
    if !element.children.is_empty() {
        return Err(format!("<{}> must contain text only", element.name));
    }
    if element.text.trim().is_empty() {
        return Err(format!("<{}> must not be empty", element.name));
    }
    Ok(())
}

fn expect_integer(element: &DmarcElement) -> Result<(), String> {
    expect_text(element)?;
    element
        .text
        .trim()
        .parse::<i64>()
        .map(|_| ())
        .map_err(|_| format!("<{}> must contain an integer", element.name))
}

fn expect_decimal(element: &DmarcElement) -> Result<(), String> {
    expect_text(element)?;
    element
        .text
        .trim()
        .parse::<f64>()
        .map(|_| ())
        .map_err(|_| format!("<{}> must contain a decimal", element.name))
}

fn expect_enum(element: &DmarcElement, allowed: &[&str]) -> Result<(), String> {
    expect_text(element)?;
    let value = element.text.trim();
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "<{}> contains invalid value '{}'",
            element.name, value
        ))
    }
}

fn require_child<'a>(element: &'a DmarcElement, name: &str) -> Result<&'a DmarcElement, String> {
    child(element, name)
        .ok_or_else(|| format!("<{}> is missing required <{}> element", element.name, name))
}

fn child<'a>(element: &'a DmarcElement, name: &str) -> Option<&'a DmarcElement> {
    element.children.iter().find(|child| child.name == name)
}

fn children<'a>(
    element: &'a DmarcElement,
    name: &'a str,
) -> impl Iterator<Item = &'a DmarcElement> {
    element
        .children
        .iter()
        .filter(move |child| child.name == name)
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
    fn rejects_xml_that_does_not_match_dmarc_schema() {
        let input = ReportInput {
            source: "fixture:invalid.xml".to_owned(),
            file_name: "invalid.xml".to_owned(),
            message_sender: "noreply@example.com".to_owned(),
            bytes: br#"<?xml version="1.0" encoding="UTF-8"?>
<feedback>
  <report_metadata>
    <org_name>Example</org_name>
    <email>noreply@example.com</email>
  </report_metadata>
  <policy_published>
    <domain>example.com</domain>
    <p>none</p>
  </policy_published>
  <record>
    <row>
      <source_ip>192.0.2.1</source_ip>
      <count>1</count>
      <policy_evaluated>
        <disposition>none</disposition>
        <dkim>pass</dkim>
        <spf>pass</spf>
      </policy_evaluated>
    </row>
    <identifiers>
      <header_from>example.com</header_from>
    </identifiers>
    <auth_results>
      <spf>
        <domain>example.com</domain>
        <result>pass</result>
      </spf>
    </auth_results>
  </record>
</feedback>"#
                .to_vec(),
        };

        let err = scan_report_inputs(&[input], 1, default_zip_limits())
            .expect_err("schema validation should reject missing report_id/date_range");

        assert!(err.contains("XML schema validation failed"));
        assert!(err.contains("report_id"));
    }

    #[test]
    fn rejects_invalid_policy_published_domain() {
        let input = ReportInput {
            source: "fixture:invalid-domain.xml".to_owned(),
            file_name: "invalid-domain.xml".to_owned(),
            message_sender: "noreply@example.com".to_owned(),
            bytes: dmarc_xml_with_domain("noreply@example.com", "bad_domain").into_bytes(),
        };

        let err = scan_report_inputs(&[input], 1, default_zip_limits())
            .expect_err("invalid report domain should fail");

        assert!(err.contains("Invalid policy_published/domain"));
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
        dmarc_xml_with_domain(email, "example.com")
    }

    fn dmarc_xml_with_domain(email: &str, domain: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<feedback>
  <report_metadata>
    <org_name>Example</org_name>
    <email>{email}</email>
    <report_id>synthetic-report</report_id>
    <date_range>
      <begin>1774656000</begin>
      <end>1774742399</end>
    </date_range>
  </report_metadata>
  <policy_published>
    <domain>{domain}</domain>
    <p>none</p>
    <pct>100</pct>
  </policy_published>
  <record>
    <row>
      <source_ip>192.0.2.1</source_ip>
      <count>1</count>
      <policy_evaluated>
        <disposition>none</disposition>
        <dkim>pass</dkim>
        <spf>pass</spf>
      </policy_evaluated>
    </row>
    <identifiers>
      <header_from>example.com</header_from>
    </identifiers>
    <auth_results>
      <spf>
        <domain>example.com</domain>
        <result>pass</result>
      </spf>
    </auth_results>
  </record>
</feedback>"#
        )
    }
}
