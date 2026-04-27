use serde::Deserialize;
use std::fs;
use std::str::FromStr;

#[derive(Debug, Deserialize)]
pub(crate) struct AppConfig {
    pub(crate) mqtt: MqttConfig,
    pub(crate) imap: ImapConfig,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MqttConfig {
    pub(crate) server_name: String,
    pub(crate) server_port: u16,
    pub(crate) login: String,
    pub(crate) password: String,
    pub(crate) base_topic: String,
    #[serde(default)]
    pub(crate) remove_stale_sensors: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ImapConfig {
    pub(crate) mailboxes: Vec<ImapMailboxConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct ImapMailboxConfig {
    pub(crate) name: String,
    pub(crate) server_name: String,
    pub(crate) server_port: u16,
    pub(crate) login: String,
    pub(crate) password: String,
    pub(crate) report_folder: String,
    pub(crate) trash_folder: String,
    pub(crate) move_emails: bool,
    #[serde(default = "default_poll_cron")]
    pub(crate) poll_cron: String,
    #[serde(default = "default_max_xml_size")]
    pub(crate) max_xml_size: u64,
}

fn default_poll_cron() -> String {
    "0 0 */6 * * *".to_owned()
}

fn default_max_xml_size() -> u64 {
    10
}

pub(crate) fn load_config(path: &str) -> Result<AppConfig, String> {
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
    if matches!(config.mqtt.remove_stale_sensors, Some(0)) {
        return Err(
            "Config field mqtt.remove_stale_sensors must be greater than 0 when set".to_owned(),
        );
    }
    validate_mailboxes(&config.imap.mailboxes)?;

    Ok(config)
}

fn validate_mailboxes(mailboxes: &[ImapMailboxConfig]) -> Result<(), String> {
    if mailboxes.is_empty() {
        return Err("Config field imap.mailboxes must contain at least one mailbox".to_owned());
    }

    let mut seen_names = std::collections::BTreeSet::new();
    for mailbox in mailboxes {
        let mailbox_name = mailbox.name.trim();
        if mailbox_name.is_empty() {
            return Err("Config field imap.mailboxes[].name must not be empty".to_owned());
        }
        if !seen_names.insert(mailbox_name.to_owned()) {
            return Err(format!(
                "Config field imap.mailboxes contains duplicate mailbox name '{}'",
                mailbox_name
            ));
        }
        if mailbox.server_name.trim().is_empty() {
            return Err(format!(
                "Config field imap.mailboxes['{}'].server_name must not be empty",
                mailbox_name
            ));
        }
        if mailbox.login.trim().is_empty() {
            return Err(format!(
                "Config field imap.mailboxes['{}'].login must not be empty",
                mailbox_name
            ));
        }
        if mailbox.password.trim().is_empty() {
            return Err(format!(
                "Config field imap.mailboxes['{}'].password must not be empty",
                mailbox_name
            ));
        }
        if mailbox.report_folder.trim().is_empty() {
            return Err(format!(
                "Config field imap.mailboxes['{}'].report_folder must not be empty",
                mailbox_name
            ));
        }
        if mailbox.trash_folder.trim().is_empty() {
            return Err(format!(
                "Config field imap.mailboxes['{}'].trash_folder must not be empty",
                mailbox_name
            ));
        }
        if mailbox.server_port == 0 {
            return Err(format!(
                "Config field imap.mailboxes['{}'].server_port must be greater than 0",
                mailbox_name
            ));
        }
        if mailbox.poll_cron.trim().is_empty() {
            return Err(format!(
                "Config field imap.mailboxes['{}'].poll_cron must not be empty",
                mailbox_name
            ));
        }
        if mailbox.max_xml_size == 0 {
            return Err(format!(
                "Config field imap.mailboxes['{}'].max_xml_size must be greater than 0",
                mailbox_name
            ));
        }
        cron::Schedule::from_str(&mailbox.poll_cron).map_err(|err| {
            format!(
                "Config field imap.mailboxes['{}'].poll_cron is invalid ({}): {err}",
                mailbox_name, mailbox.poll_cron
            )
        })?;
    }

    Ok(())
}
