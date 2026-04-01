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
    if config.imap.server_name.trim().is_empty() {
        return Err("Config field imap.server_name must not be empty".to_owned());
    }
    if config.imap.login.trim().is_empty() {
        return Err("Config field imap.login must not be empty".to_owned());
    }
    if config.imap.password.trim().is_empty() {
        return Err("Config field imap.password must not be empty".to_owned());
    }
    if config.imap.report_folder.trim().is_empty() {
        return Err("Config field imap.report_folder must not be empty".to_owned());
    }
    if config.imap.trash_folder.trim().is_empty() {
        return Err("Config field imap.trash_folder must not be empty".to_owned());
    }
    if config.imap.server_port == 0 {
        return Err("Config field imap.server_port must be greater than 0".to_owned());
    }
    if config.imap.poll_cron.trim().is_empty() {
        return Err("Config field imap.poll_cron must not be empty".to_owned());
    }
    if config.imap.max_xml_size == 0 {
        return Err("Config field imap.max_xml_size must be greater than 0".to_owned());
    }
    if matches!(config.mqtt.remove_stale_sensors, Some(0)) {
        return Err(
            "Config field mqtt.remove_stale_sensors must be greater than 0 when set".to_owned(),
        );
    }
    cron::Schedule::from_str(&config.imap.poll_cron).map_err(|err| {
        format!(
            "Config field imap.poll_cron is invalid ({}): {err}",
            config.imap.poll_cron
        )
    })?;

    Ok(config)
}
