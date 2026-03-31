use crate::mqtt::AppConfig;
use mailparse::ParsedMail;
use rustls_connector::RustlsConnector;
use std::net::TcpStream;

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

pub(crate) fn fetch_messages_with_attachments(config: &AppConfig) -> Result<Vec<MailMessage>, String> {
    let mut session = connect_and_login(config)?;
    let folder = folder_path(config);
    session
        .select(&folder)
        .map_err(|err| format!("Failed to select IMAP folder {folder}: {err}"))?;

    let ids = session
        .uid_search("ALL")
        .map_err(|err| format!("Failed to search IMAP folder {folder}: {err}"))?;

    let mut sorted_ids: Vec<u32> = ids.into_iter().collect();
    sorted_ids.sort_unstable();

    let mut messages = Vec::new();
    for uid in sorted_ids {
        let fetches = session
            .uid_fetch(uid.to_string(), "RFC822")
            .map_err(|err| format!("Failed to fetch IMAP message UID {uid}: {err}"))?;

        for fetch in &fetches {
            if let Some(raw) = fetch.body() {
                let attachments = extract_attachment_inputs(raw, &folder, uid)?;
                messages.push(MailMessage { uid, attachments });
            }
        }
    }

    session
        .logout()
        .map_err(|err| format!("Failed to logout from IMAP session: {err}"))?;

    Ok(messages)
}

pub(crate) fn move_message_to_trash(config: &AppConfig, uid: u32) -> Result<(), String> {
    let mut session = connect_and_login(config)?;
    let source_folder = folder_path(config);
    session
        .select(&source_folder)
        .map_err(|err| format!("Failed to select IMAP folder {source_folder}: {err}"))?;

    session
        .uid_copy(uid.to_string(), &config.imap.trash_folder)
        .map_err(|err| {
            format!(
                "Failed to copy IMAP message UID {uid} to trash folder {}: {err}",
                config.imap.trash_folder
            )
        })?;

    session
        .uid_store(uid.to_string(), "+FLAGS (\\Deleted)")
        .map_err(|err| format!("Failed to mark IMAP message UID {uid} as deleted: {err}"))?;

    if session.uid_expunge(uid.to_string()).is_err() {
        session
            .expunge()
            .map_err(|err| format!("Failed to expunge IMAP message UID {uid}: {err}"))?;
    }

    session
        .logout()
        .map_err(|err| format!("Failed to logout from IMAP session: {err}"))?;

    Ok(())
}

fn connect_and_login(
    config: &AppConfig,
) -> Result<imap::Session<rustls_connector::TlsStream<TcpStream>>, String> {
    let stream = TcpStream::connect((config.imap.server_name.as_str(), config.imap.server_port)).map_err(
        |err| {
            format!(
                "Failed to connect to IMAP {}:{}: {err}",
                config.imap.server_name, config.imap.server_port
            )
        },
    )?;

    let tls = RustlsConnector::new_with_native_certs()
        .map_err(|err| format!("Failed to load native TLS certificates: {err}"))?;

    let tls_stream = tls
        .connect(&config.imap.server_name, stream)
        .map_err(|err| format!("Failed to establish IMAP TLS connection: {err}"))?;

    let client = imap::Client::new(tls_stream);
    client
        .login(&config.imap.login, &config.imap.password)
        .map_err(|(err, _)| format!("Failed to login to IMAP: {err}"))
}

fn folder_path(config: &AppConfig) -> String {
    config.imap.report_folder.clone()
}

fn extract_attachment_inputs(
    raw_message: &[u8],
    folder: &str,
    message_uid: u32,
) -> Result<Vec<ReportInput>, String> {
    let parsed = mailparse::parse_mail(raw_message)
        .map_err(|err| format!("Failed to parse MIME message UID {message_uid}: {err}"))?;

    let mut out = Vec::new();
    collect_attachments(&parsed, folder, message_uid, &mut out)?;
    Ok(out)
}

fn collect_attachments(
    part: &ParsedMail,
    folder: &str,
    message_uid: u32,
    out: &mut Vec<ReportInput>,
) -> Result<(), String> {
    if !part.subparts.is_empty() {
        for child in &part.subparts {
            collect_attachments(child, folder, message_uid, out)?;
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
        .map_err(|err| format!("Failed to decode attachment in message UID {message_uid}: {err}"))?;

    out.push(ReportInput {
        source: format!("imap:{folder}#{message_uid}:{file_name}"),
        file_name,
        bytes,
    });

    Ok(())
}
