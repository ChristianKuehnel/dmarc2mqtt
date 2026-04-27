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
    #[serde(default = "default_mqtt_tls")]
    pub(crate) tls: bool,
    #[serde(default)]
    pub(crate) allow_insecure: bool,
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
    #[serde(default = "default_max_message_size")]
    pub(crate) max_message_size: u64,
    #[serde(default = "default_max_attachment_size")]
    pub(crate) max_attachment_size: u64,
}

fn default_poll_cron() -> String {
    "0 0 */6 * * *".to_owned()
}

fn default_max_xml_size() -> u64 {
    10
}

fn default_max_message_size() -> u64 {
    25
}

fn default_max_attachment_size() -> u64 {
    10
}

fn default_mqtt_tls() -> bool {
    true
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
    if !config.mqtt.tls
        && !config.mqtt.allow_insecure
        && (!config.mqtt.login.trim().is_empty() || !config.mqtt.password.trim().is_empty())
    {
        return Err(
            "MQTT credentials require TLS. Set mqtt.tls: true or explicitly set mqtt.allow_insecure: true to use plaintext MQTT.".to_owned(),
        );
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
        if mailbox.server_port == 143 {
            return Err(format!(
                "Config field imap.mailboxes['{}'].server_port uses plaintext IMAP port 143. Configure IMAPS/TLS, typically port 993.",
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
        if mailbox.max_message_size == 0 {
            return Err(format!(
                "Config field imap.mailboxes['{}'].max_message_size must be greater than 0",
                mailbox_name
            ));
        }
        if mailbox.max_attachment_size == 0 {
            return Err(format!(
                "Config field imap.mailboxes['{}'].max_attachment_size must be greater than 0",
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn write_config(content: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "dmarc2mqtt-config-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("test config directory should be created");
        let path = dir.join("config.yaml");
        fs::write(&path, content).expect("test config should be written");
        path
    }

    fn config_yaml(mqtt_extra: &str) -> String {
        format!(
            r#"mqtt:
  server_name: "mqtt.example.local"
  server_port: 8883
  login: "dmarc2mqtt"
  password: "change-me"
  base_topic: "mail/dmarc"
{mqtt_extra}imap:
  mailboxes:
    - name: "primary"
      server_name: "imap.example.com"
      server_port: 993
      login: "user@example.com"
      password: "change-me"
      report_folder: "INBOX/DMARC"
      trash_folder: "INBOX/Trash"
      move_emails: false
"#
        )
    }

    #[test]
    fn defaults_mqtt_to_tls() {
        let path = write_config(&config_yaml(""));
        let config = load_config(path.to_str().expect("test path should be valid unicode"))
            .expect("config should load");

        assert!(config.mqtt.tls);
        assert!(!config.mqtt.allow_insecure);
        assert_eq!(config.imap.mailboxes[0].max_message_size, 25);
        assert_eq!(config.imap.mailboxes[0].max_attachment_size, 10);
    }

    #[test]
    fn rejects_plaintext_mqtt_with_credentials_without_opt_in() {
        let path = write_config(&config_yaml("  tls: false\n"));
        let err = load_config(path.to_str().expect("test path should be valid unicode"))
            .expect_err("plaintext MQTT credentials should be rejected");

        assert!(err.contains("MQTT credentials require TLS"));
    }

    #[test]
    fn allows_plaintext_mqtt_with_explicit_opt_in() {
        let path = write_config(&config_yaml("  tls: false\n  allow_insecure: true\n"));
        let config = load_config(path.to_str().expect("test path should be valid unicode"))
            .expect("explicit insecure MQTT opt-in should load");

        assert!(!config.mqtt.tls);
        assert!(config.mqtt.allow_insecure);
    }

    #[test]
    fn rejects_plaintext_imap_port() {
        let path = write_config(&config_yaml("").replacen(
            "      server_port: 993",
            "      server_port: 143",
            1,
        ));
        let err = load_config(path.to_str().expect("test path should be valid unicode"))
            .expect_err("plaintext IMAP port should be rejected");

        assert!(err.contains("plaintext IMAP port 143"));
    }

    #[test]
    fn rejects_zero_imap_size_limits() {
        let path = write_config(&format!("{}      max_message_size: 0\n", config_yaml("")));
        let err = load_config(path.to_str().expect("test path should be valid unicode"))
            .expect_err("zero max_message_size should be rejected");

        assert!(err.contains("max_message_size"));

        let path = write_config(&format!(
            "{}      max_attachment_size: 0\n",
            config_yaml("")
        ));
        let err = load_config(path.to_str().expect("test path should be valid unicode"))
            .expect_err("zero max_attachment_size should be rejected");

        assert!(err.contains("max_attachment_size"));
    }
}
