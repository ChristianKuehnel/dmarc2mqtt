use crate::config::ImapMailboxConfig;
use mailparse::ParsedMail;

#[derive(Debug, Clone)]
pub(crate) struct ReportInput {
    pub(crate) source: String,
    pub(crate) file_name: String,
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
    session
        .select(&folder)
        .map_err(|err| {
            format!(
                "Failed to select IMAP folder {} for mailbox '{}': {err}",
                folder, mailbox.name
            )
        })?;

    let ids = session
        .uid_search("ALL")
        .map_err(|err| {
            format!(
                "Failed to search IMAP folder {} for mailbox '{}': {err}",
                folder, mailbox.name
            )
        })?;

    let mut sorted_ids: Vec<u32> = ids.into_iter().collect();
    sorted_ids.sort_unstable();

    let mut messages = Vec::new();
    for uid in sorted_ids {
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
                let attachments = extract_attachment_inputs(raw, mailbox, &folder, uid)?;
                messages.push(MailMessage { uid, attachments });
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
    session
        .select(&source_folder)
        .map_err(|err| {
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
        .connect()
        .map_err(|err| {
            format!(
                "Failed to establish IMAP TLS connection to {}:{} for mailbox '{}': {err}",
                mailbox.server_name, mailbox.server_port, mailbox.name
            )
        })?;
    client
        .login(&mailbox.login, &mailbox.password)
        .map_err(|(err, _)| format!("Failed to login to IMAP for mailbox '{}': {err}", mailbox.name))
}

fn folder_path(mailbox: &ImapMailboxConfig) -> String {
    mailbox.report_folder.clone()
}

fn extract_attachment_inputs(
    raw_message: &[u8],
    mailbox: &ImapMailboxConfig,
    folder: &str,
    message_uid: u32,
) -> Result<Vec<ReportInput>, String> {
    let parsed = mailparse::parse_mail(raw_message)
        .map_err(|err| {
            format!(
                "Failed to parse MIME message UID {message_uid} for mailbox '{}': {err}",
                mailbox.name
            )
        })?;

    let mut out = Vec::new();
    collect_attachments(&parsed, mailbox, folder, message_uid, &mut out)?;
    Ok(out)
}

fn collect_attachments(
    part: &ParsedMail,
    mailbox: &ImapMailboxConfig,
    folder: &str,
    message_uid: u32,
    out: &mut Vec<ReportInput>,
) -> Result<(), String> {
    if !part.subparts.is_empty() {
        for child in &part.subparts {
            collect_attachments(child, mailbox, folder, message_uid, out)?;
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
    let bytes = part
        .get_body_raw()
        .map_err(|err| {
            format!(
                "Failed to decode attachment in message UID {message_uid} for mailbox '{}': {err}",
                mailbox.name
            )
        })?;

    out.push(ReportInput {
        source: format!(
            "imap:{}:{}:{}#{message_uid}:{file_name}",
            mailbox.name, mailbox.server_name, folder
        ),
        file_name,
        bytes,
    });

    Ok(())
}
