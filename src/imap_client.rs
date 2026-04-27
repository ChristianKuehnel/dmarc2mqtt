use crate::config::ImapMailboxConfig;
use log::warn;
use mail_auth::common::verify::VerifySignature;
use mail_auth::{AuthenticatedMessage, DkimResult, MessageAuthenticator};
use mailparse::{body::Body, MailAddr, MailHeaderMap, ParsedMail};

#[derive(Debug, Clone)]
pub(crate) struct ReportInput {
    pub(crate) source: String,
    pub(crate) file_name: String,
    pub(crate) message_sender: String,
    pub(crate) bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub(crate) struct MailMessage {
    pub(crate) uid: u32,
    pub(crate) attachments: Vec<ReportInput>,
}

pub(crate) fn fetch_messages_with_attachments(
    mailbox: &ImapMailboxConfig,
) -> Result<Vec<MailMessage>, String> {
    let mut session = connect_and_login(mailbox)?;
    let folder = folder_path(mailbox);
    let max_message_bytes = size_limit_bytes(mailbox.max_message_size, "max_message_size")?;
    session.select(&folder).map_err(|err| {
        format!(
            "Failed to select IMAP folder {} for mailbox '{}': {err}",
            folder, mailbox.name
        )
    })?;

    let ids = session.uid_search("ALL").map_err(|err| {
        format!(
            "Failed to search IMAP folder {} for mailbox '{}': {err}",
            folder, mailbox.name
        )
    })?;

    let mut sorted_ids: Vec<u32> = ids.into_iter().collect();
    sorted_ids.sort_unstable();

    let mut messages = Vec::new();
    for uid in sorted_ids {
        if message_exceeds_size_limit(&mut session, mailbox, uid, max_message_bytes)? {
            continue;
        }

        let fetches = session
            .uid_fetch(uid.to_string(), "RFC822")
            .map_err(|err| {
                format!(
                    "Failed to fetch IMAP message UID {uid} for mailbox '{}': {err}",
                    mailbox.name
                )
            })?;

        for fetch in fetches.iter() {
            if let Some(raw) = fetch.body() {
                if raw.len() > max_message_bytes {
                    warn!(
                        "Skipping IMAP message UID {uid} for mailbox '{}': fetched message size {} byte(s) exceeds max_message_size ({} MB).",
                        mailbox.name,
                        raw.len(),
                        mailbox.max_message_size
                    );
                    continue;
                }
                let attachments = extract_attachment_inputs(raw, mailbox, &folder, uid)?;
                if !attachments.is_empty() {
                    messages.push(MailMessage { uid, attachments });
                }
            }
        }
    }

    session
        .logout()
        .map_err(|err| format!("Failed to logout from IMAP session: {err}"))?;

    Ok(messages)
}

pub(crate) fn move_message_to_trash(mailbox: &ImapMailboxConfig, uid: u32) -> Result<(), String> {
    let mut session = connect_and_login(mailbox)?;
    let source_folder = folder_path(mailbox);
    session.select(&source_folder).map_err(|err| {
        format!(
            "Failed to select IMAP folder {} for mailbox '{}': {err}",
            source_folder, mailbox.name
        )
    })?;

    session
        .uid_mv(uid.to_string(), &mailbox.trash_folder)
        .map_err(|err| {
            format!(
                "Failed to move IMAP message UID {uid} to trash folder {} for mailbox '{}': {err}",
                mailbox.trash_folder, mailbox.name
            )
        })?;

    session
        .logout()
        .map_err(|err| format!("Failed to logout from IMAP session: {err}"))?;

    Ok(())
}

fn connect_and_login(
    mailbox: &ImapMailboxConfig,
) -> Result<imap::Session<imap::Connection>, String> {
    let client = imap::ClientBuilder::new(&mailbox.server_name, mailbox.server_port)
        .tls_kind(imap::TlsKind::Rust)
        .connect()
        .map_err(|err| {
            format!(
                "Failed to establish IMAP TLS connection to {}:{} for mailbox '{}': {err}",
                mailbox.server_name, mailbox.server_port, mailbox.name
            )
        })?;
    client
        .login(&mailbox.login, &mailbox.password)
        .map_err(|(err, _)| {
            format!(
                "Failed to login to IMAP for mailbox '{}': {err}",
                mailbox.name
            )
        })
}

fn folder_path(mailbox: &ImapMailboxConfig) -> String {
    mailbox.report_folder.clone()
}

fn message_exceeds_size_limit(
    session: &mut imap::Session<imap::Connection>,
    mailbox: &ImapMailboxConfig,
    uid: u32,
    max_message_bytes: usize,
) -> Result<bool, String> {
    let fetches = session
        .uid_fetch(uid.to_string(), "RFC822.SIZE")
        .map_err(|err| {
            format!(
                "Failed to fetch IMAP message size for UID {uid} in mailbox '{}': {err}",
                mailbox.name
            )
        })?;

    for fetch in fetches.iter() {
        if let Some(size) = fetch.size {
            if size as u64 > max_message_bytes as u64 {
                warn!(
                    "Skipping IMAP message UID {uid} for mailbox '{}': message size {} byte(s) exceeds max_message_size ({} MB).",
                    mailbox.name,
                    size,
                    mailbox.max_message_size
                );
                return Ok(true);
            }
        }
    }

    Ok(false)
}

fn extract_attachment_inputs(
    raw_message: &[u8],
    mailbox: &ImapMailboxConfig,
    folder: &str,
    message_uid: u32,
) -> Result<Vec<ReportInput>, String> {
    let max_attachment_bytes =
        size_limit_bytes(mailbox.max_attachment_size, "max_attachment_size")?;
    let parsed = mailparse::parse_mail(raw_message).map_err(|err| {
        format!(
            "Failed to parse MIME message UID {message_uid} for mailbox '{}': {err}",
            mailbox.name
        )
    })?;
    let message_sender = message_sender(&parsed, mailbox, message_uid)?;

    let mut out = Vec::new();
    collect_attachments(
        &parsed,
        mailbox,
        folder,
        message_uid,
        &message_sender,
        max_attachment_bytes,
        &mut out,
    )?;
    if !out.is_empty() {
        if let Err(err) = verify_dkim(raw_message, &message_sender) {
            warn!(
                "Skipping IMAP message UID {message_uid} for mailbox '{}': {err}",
                mailbox.name
            );
            return Ok(Vec::new());
        }
    }
    Ok(out)
}

fn collect_attachments(
    part: &ParsedMail,
    mailbox: &ImapMailboxConfig,
    folder: &str,
    message_uid: u32,
    message_sender: &str,
    max_attachment_bytes: usize,
    out: &mut Vec<ReportInput>,
) -> Result<(), String> {
    if !part.subparts.is_empty() {
        for child in &part.subparts {
            collect_attachments(
                child,
                mailbox,
                folder,
                message_uid,
                message_sender,
                max_attachment_bytes,
                out,
            )?;
        }
        return Ok(());
    }

    let disposition = part.get_content_disposition();
    let is_attachment_disposition = matches!(
        disposition.disposition,
        mailparse::DispositionType::Attachment
    );
    let file_name = disposition
        .params
        .get("filename")
        .cloned()
        .or_else(|| part.ctype.params.get("name").cloned());

    let is_attachment = is_attachment_disposition || file_name.is_some();
    if !is_attachment {
        return Ok(());
    }

    let file_name = file_name.unwrap_or_else(|| "attachment.bin".to_owned());
    let encoded_size = encoded_body_len(part);
    if encoded_size > max_attachment_bytes {
        warn!(
            "Skipping attachment {file_name} in IMAP message UID {message_uid} for mailbox '{}': encoded attachment size {} byte(s) exceeds max_attachment_size ({} MB).",
            mailbox.name,
            encoded_size,
            mailbox.max_attachment_size
        );
        return Ok(());
    }

    let bytes = part.get_body_raw().map_err(|err| {
        format!(
            "Failed to decode attachment in message UID {message_uid} for mailbox '{}': {err}",
            mailbox.name
        )
    })?;
    if bytes.len() > max_attachment_bytes {
        warn!(
            "Skipping attachment {file_name} in IMAP message UID {message_uid} for mailbox '{}': decoded attachment size {} byte(s) exceeds max_attachment_size ({} MB).",
            mailbox.name,
            bytes.len(),
            mailbox.max_attachment_size
        );
        return Ok(());
    }

    out.push(ReportInput {
        source: format!(
            "imap:{}:{}:{}#{message_uid}:{file_name}",
            mailbox.name, mailbox.server_name, folder
        ),
        file_name,
        message_sender: message_sender.to_owned(),
        bytes,
    });

    Ok(())
}

fn encoded_body_len(part: &ParsedMail) -> usize {
    match part.get_body_encoded() {
        Body::Base64(body) | Body::QuotedPrintable(body) => body.get_raw().len(),
        Body::SevenBit(body) | Body::EightBit(body) => body.get_raw().len(),
        Body::Binary(body) => body.get_raw().len(),
    }
}

fn size_limit_bytes(size_mb: u64, field_name: &str) -> Result<usize, String> {
    let bytes_u64 = size_mb
        .checked_mul(1024 * 1024)
        .ok_or_else(|| format!("Configured {field_name} is too large: {size_mb}"))?;
    usize::try_from(bytes_u64)
        .map_err(|_| format!("Configured {field_name} is too large: {size_mb}"))
}

fn message_sender(
    parsed: &ParsedMail,
    mailbox: &ImapMailboxConfig,
    message_uid: u32,
) -> Result<String, String> {
    let header = parsed
        .headers
        .get_first_header("Sender")
        .or_else(|| parsed.headers.get_first_header("From"))
        .ok_or_else(|| {
            format!(
                "Message UID {message_uid} for mailbox '{}' has no Sender or From header",
                mailbox.name
            )
        })?;

    let addresses = mailparse::addrparse_header(header).map_err(|err| {
        format!(
            "Failed to parse sender header in message UID {message_uid} for mailbox '{}': {err}",
            mailbox.name
        )
    })?;
    let mut senders = Vec::new();
    for address in addresses.iter() {
        collect_mail_addresses(address, &mut senders);
    }

    if senders.len() != 1 {
        return Err(format!(
            "Message UID {message_uid} for mailbox '{}' must have exactly one sender address, found {}",
            mailbox.name,
            senders.len()
        ));
    }

    Ok(senders.remove(0).to_ascii_lowercase())
}

fn collect_mail_addresses(address: &MailAddr, out: &mut Vec<String>) {
    match address {
        MailAddr::Single(info) => out.push(info.addr.clone()),
        MailAddr::Group(group) => {
            for address in &group.addrs {
                out.push(address.addr.clone());
            }
        }
    }
}

fn verify_dkim(raw_message: &[u8], message_sender: &str) -> Result<(), String> {
    let authenticated_message = AuthenticatedMessage::parse(raw_message)
        .ok_or_else(|| "Message could not be parsed for DKIM verification".to_owned())?;
    let sender_domain = email_domain(message_sender)
        .ok_or_else(|| format!("Message sender '{message_sender}' has no domain"))?;
    let authenticator = MessageAuthenticator::new_system_conf()
        .map_err(|err| format!("Failed to initialize DKIM DNS resolver: {err}"))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("Failed to initialize DKIM runtime: {err}"))?;
    let results = runtime.block_on(authenticator.verify_dkim(&authenticated_message));

    if results.is_empty() {
        return Err("Message has no DKIM signature".to_owned());
    }

    if results.iter().any(|result| {
        result.result() == &DkimResult::Pass
            && result.signature().is_some_and(|signature| {
                domain_aligned(sender_domain, signature.domain())
                    || email_domain(signature.identity()).is_some_and(|identity_domain| {
                        domain_aligned(sender_domain, identity_domain)
                    })
            })
    }) {
        Ok(())
    } else {
        let details = results
            .iter()
            .map(|result| result.result().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        Err(format!(
            "Message has no passing DKIM signature aligned with sender '{message_sender}' ({details})"
        ))
    }
}

fn email_domain(email: &str) -> Option<&str> {
    email
        .rsplit_once('@')
        .map(|(_, domain)| domain.trim().trim_end_matches('.'))
        .filter(|domain| !domain.is_empty())
}

fn domain_aligned(left: &str, right: &str) -> bool {
    let left = left.trim().trim_end_matches('.').to_ascii_lowercase();
    let right = right.trim().trim_end_matches('.').to_ascii_lowercase();
    left == right || left.ends_with(&format!(".{right}")) || right.ends_with(&format!(".{left}"))
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
        }
    }

    #[test]
    fn sender_header_takes_precedence_over_from() {
        let parsed = mailparse::parse_mail(
            b"From: Spoof <spoof@example.net>\r\nSender: Reports <reports@example.com>\r\n\r\nBody",
        )
        .expect("message should parse");

        let sender = message_sender(&parsed, &mailbox(), 42).expect("sender should parse");

        assert_eq!(sender, "reports@example.com");
    }

    #[test]
    fn rejects_multiple_from_addresses_without_sender() {
        let parsed = mailparse::parse_mail(b"From: one@example.com, two@example.com\r\n\r\nBody")
            .expect("message should parse");

        let err = message_sender(&parsed, &mailbox(), 42).expect_err("sender should be ambiguous");

        assert!(err.contains("exactly one sender address"));
    }

    #[test]
    fn dkim_domains_align_with_sender_domain_or_subdomain() {
        assert!(domain_aligned("example.com", "example.com"));
        assert!(domain_aligned("reports.example.com", "example.com"));
        assert!(domain_aligned("example.com", "mail.example.com"));
        assert!(!domain_aligned("example.com", "example.net"));
        assert!(!domain_aligned("badexample.com", "example.com"));
    }

    #[test]
    fn skips_attachment_that_exceeds_encoded_size_limit_before_decode() {
        let parsed = mailparse::parse_mail(
            b"Content-Type: application/xml; name=\"report.xml\"\r\nContent-Disposition: attachment; filename=\"report.xml\"\r\nContent-Transfer-Encoding: base64\r\n\r\ndGVzdA==",
        )
        .expect("message part should parse");
        let mut out = Vec::new();

        collect_attachments(
            &parsed,
            &mailbox(),
            "INBOX/DMARC",
            42,
            "reports@example.com",
            4,
            &mut out,
        )
        .expect("oversized attachment should be skipped, not fail");

        assert!(out.is_empty());
    }
}
