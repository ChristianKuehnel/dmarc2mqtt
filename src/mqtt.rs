use serde::Deserialize;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::AggregatedStatus;
use crate::DomainStatus;
use crate::history::KnownTuple;

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
}

fn default_poll_cron() -> String {
    "0 0 */6 * * *".to_owned()
}

#[derive(Serialize)]
struct HomeAssistantSensorConfig {
    name: String,
    unique_id: String,
    state_topic: String,
    icon: String,
    object_id: String,
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

pub(crate) fn publish_reports_to_mqtt(
    config: &AppConfig,
    aggregated_statuses: &[AggregatedStatus],
    domain_statuses: &[DomainStatus],
    known_tuples: &[KnownTuple],
) -> Result<(), String> {
    let mut mqtt_options = rumqttc::MqttOptions::new(
        "dmarc2mqtt",
        config.mqtt.server_name.clone(),
        config.mqtt.server_port,
    );
    mqtt_options.set_credentials(config.mqtt.login.clone(), config.mqtt.password.clone());
    mqtt_options.set_keep_alive(Duration::from_secs(10));

    let (client, mut connection) = rumqttc::Client::new(mqtt_options, 20);
    let running = Arc::new(AtomicBool::new(true));
    let connection_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let running_bg = Arc::clone(&running);
    let connection_error_bg = Arc::clone(&connection_error);

    let network_thread = thread::spawn(move || {
        for notification in connection.iter() {
            if !running_bg.load(Ordering::Relaxed) {
                break;
            }
            if let Err(err) = notification {
                if let Ok(mut slot) = connection_error_bg.lock() {
                    *slot = Some(err.to_string());
                }
                break;
            }
        }
    });

    let mut known_sensor_keys: BTreeMap<(String, String), (String, String)> = BTreeMap::new();
    for item in known_tuples {
        known_sensor_keys.insert(
            (item.org_name.clone(), item.domain.clone()),
            (item.org_name.clone(), item.domain.clone()),
        );
    }
    for item in aggregated_statuses {
        known_sensor_keys.insert(
            (item.org_name.clone(), item.domain.clone()),
            (item.org_name.clone(), item.domain.clone()),
        );
    }

    for (_, (org_name, domain)) in known_sensor_keys {
        let object_suffix = slugify(&format!("{org_name}_{domain}"));
        let object_id = format!("dmarc2mqtt_{object_suffix}");
        let discovery_topic = format!("homeassistant/sensor/{object_id}/config");
        let state_topic = format!("{}/sensors/{object_suffix}/state", config.mqtt.base_topic);

        let config_payload = serde_json::to_vec(&HomeAssistantSensorConfig {
            name: format!("DMARC {org_name} {domain}"),
            unique_id: object_id.clone(),
            state_topic,
            icon: "mdi:email-check-outline".to_owned(),
            object_id,
        })
        .map_err(|err| format!("Failed to serialize discovery payload: {err}"))?;
        client
            .publish(discovery_topic, rumqttc::QoS::AtLeastOnce, true, config_payload)
            .map_err(|err| format!("Failed to publish Home Assistant discovery: {err}"))?;
    }

    for item in aggregated_statuses {
        let object_suffix = slugify(&format!("{}_{}", item.org_name, item.domain));
        let state_topic = format!("{}/sensors/{}/state", config.mqtt.base_topic, object_suffix);
        client
            .publish(
                state_topic,
                rumqttc::QoS::AtLeastOnce,
                true,
                item.status.clone(),
            )
            .map_err(|err| format!("Failed to publish sensor state: {err}"))?;
    }

    let mut known_domains: BTreeSet<String> = BTreeSet::new();
    for item in known_tuples {
        known_domains.insert(item.domain.clone());
    }
    for item in domain_statuses {
        known_domains.insert(item.domain.clone());
    }

    for domain in known_domains {
        let object_suffix = slugify(&domain);
        let object_id = format!("dmarc2mqtt_domain_{object_suffix}");
        let discovery_topic = format!("homeassistant/sensor/{object_id}/config");
        let state_topic = format!("{}/domains/{object_suffix}/state", config.mqtt.base_topic);

        let config_payload = serde_json::to_vec(&HomeAssistantSensorConfig {
            name: format!("DMARC Domain {domain}"),
            unique_id: object_id.clone(),
            state_topic,
            icon: "mdi:shield-check-outline".to_owned(),
            object_id,
        })
        .map_err(|err| format!("Failed to serialize domain discovery payload: {err}"))?;
        client
            .publish(discovery_topic, rumqttc::QoS::AtLeastOnce, true, config_payload)
            .map_err(|err| format!("Failed to publish domain discovery: {err}"))?;
    }

    for item in domain_statuses {
        let object_suffix = slugify(&item.domain);
        let state_topic = format!("{}/domains/{object_suffix}/state", config.mqtt.base_topic);
        client
            .publish(
                state_topic,
                rumqttc::QoS::AtLeastOnce,
                true,
                item.status.clone(),
            )
            .map_err(|err| format!("Failed to publish domain state: {err}"))?;
    }

    thread::sleep(Duration::from_millis(300));
    let _ = client.disconnect();
    running.store(false, Ordering::Relaxed);
    let _ = network_thread.join();

    if let Ok(slot) = connection_error.lock() {
        if let Some(err) = &*slot {
            return Err(format!("MQTT connection error: {err}"));
        }
    }

    Ok(())
}

fn slugify(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut prev_underscore = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            output.push(ch.to_ascii_lowercase());
            prev_underscore = false;
        } else if !prev_underscore {
            output.push('_');
            prev_underscore = true;
        }
    }

    let trimmed = output.trim_matches('_').to_owned();
    if trimmed.is_empty() {
        "unknown".to_owned()
    } else {
        trimmed
    }
}
