use serde::Deserialize;
use serde::Serialize;
use std::fs;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::AggregatedStatus;
use crate::DomainStatus;

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
}

#[derive(Debug, Deserialize)]
pub(crate) struct ImapConfig {
    pub(crate) server_name: String,
    pub(crate) server_port: u16,
    pub(crate) login: String,
    pub(crate) password: String,
    pub(crate) report_folder: String,
    pub(crate) trash_folder: String,
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

    Ok(config)
}

pub(crate) fn publish_reports_to_mqtt(
    config: &AppConfig,
    aggregated_statuses: &[AggregatedStatus],
    domain_statuses: &[DomainStatus],
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

    for item in aggregated_statuses {
        let object_suffix = slugify(&format!("{}_{}", item.org_name, item.domain));
        let object_id = format!("dmarc2mqtt_{}", object_suffix);
        let discovery_topic = format!("homeassistant/sensor/{object_id}/config");
        let state_topic = format!("{}/sensors/{}/state", config.mqtt.base_topic, object_suffix);

        let config_payload = serde_json::to_vec(&HomeAssistantSensorConfig {
            name: format!("DMARC {} {}", item.org_name, item.domain),
            unique_id: object_id.clone(),
            state_topic: state_topic.clone(),
            icon: "mdi:email-check-outline".to_owned(),
            object_id,
        })
        .map_err(|err| format!("Failed to serialize discovery payload: {err}"))?;
        client
            .publish(discovery_topic, rumqttc::QoS::AtLeastOnce, true, config_payload)
            .map_err(|err| format!("Failed to publish Home Assistant discovery: {err}"))?;

        client
            .publish(
                state_topic,
                rumqttc::QoS::AtLeastOnce,
                true,
                item.status.clone(),
            )
            .map_err(|err| format!("Failed to publish sensor state: {err}"))?;
    }

    for item in domain_statuses {
        let object_suffix = slugify(&item.domain);
        let object_id = format!("dmarc2mqtt_domain_{}", object_suffix);
        let discovery_topic = format!("homeassistant/sensor/{object_id}/config");
        let state_topic = format!("{}/domains/{}/state", config.mqtt.base_topic, object_suffix);

        let config_payload = serde_json::to_vec(&HomeAssistantSensorConfig {
            name: format!("DMARC Domain {}", item.domain),
            unique_id: object_id.clone(),
            state_topic: state_topic.clone(),
            icon: "mdi:shield-check-outline".to_owned(),
            object_id,
        })
        .map_err(|err| format!("Failed to serialize domain discovery payload: {err}"))?;
        client
            .publish(discovery_topic, rumqttc::QoS::AtLeastOnce, true, config_payload)
            .map_err(|err| format!("Failed to publish domain discovery: {err}"))?;

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
