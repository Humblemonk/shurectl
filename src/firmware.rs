//! Looks up the newest firmware Shure has released for each model, so the Info
//! tab can say whether the connected device is up to date. Shure ships these
//! updates only through its MOTIV apps; shurectl never writes firmware.
//!
//! Shure's firmware archive page (shure.com/en-US/support/downloads/
//! software-firmware-archive) loads its list from an Elastic App Search engine
//! with a public search key. The endpoint and key below were taken from the
//! page's JavaScript on 2026-10-20 and can change without notice, so any
//! failure leaves the Info tab showing only the installed version.

use std::collections::HashMap;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::protocol::{DeviceModel, parse_firmware};

const ENDPOINT: &str = "https://initial2022.ent.eu-west-1.aws.found.io/api/as/v1/engines/redesign-meta-prod/search.json";
const SEARCH_KEY: &str = "search-fbxbkqcj2rc5xbe2y3itp812";
const TIMEOUT: Duration = Duration::from_secs(10);

/// Every archive item `archive_item()` returns. The request names them all, so
/// it doesn't say which model is connected.
const ARCHIVE_ITEMS: [&str; 4] = ["mvx2u-gen2", "MV6", "mv6-gen2", "mv7plus"];

/// The archive's item name for a model's firmware, `None` where the version the
/// device reports can't be compared with the archive's.
fn archive_item(model: DeviceModel) -> Option<&'static str> {
    match model {
        // Unconfirmed what version string the Gen 1 reports; a scheme like the
        // MV7's would tell every Gen 1 an update is available.
        DeviceModel::Mvx2u => None,
        DeviceModel::Mvx2uGen2 => Some("mvx2u-gen2"),
        DeviceModel::Mv6 => Some("MV6"),
        DeviceModel::Mv6Gen2 => Some("mv6-gen2"),
        // The MV7's text shell reports 0.0.52.0 for what the archive lists as
        // package 1.2.19 (see protocol.rs `mv7_text`); one known pair can't map
        // the others.
        DeviceModel::Mv7 => None,
        DeviceModel::Mv7Plus => Some("mv7plus"),
    }
}

/// Whether the Info tab can show a firmware status for this model, so the
/// lookup is worth making.
pub fn is_compared(model: DeviceModel) -> bool {
    archive_item(model).is_some()
}

/// How the installed firmware compares with the newest the archive lists.
#[derive(Debug, PartialEq)]
pub enum FirmwareStatus<'a> {
    UpToDate,
    /// Holds the newest version, as the archive writes it.
    UpdateAvailable(&'a str),
}

/// The newest firmware version the archive lists for each archive item.
#[derive(Debug, Default)]
pub struct LatestFirmware {
    versions: HashMap<String, String>,
}

impl LatestFirmware {
    /// `None` when the installed version is unknown, the model isn't compared,
    /// or the archive lists nothing for it.
    ///
    /// The archive lists three-part versions ("1.1.0") and devices report four
    /// ("1.1.0.21"); padded to four, the archive's release compares at or below
    /// every build of it. Versions the devices report, all matching the
    /// archive's first three parts: MV6 Gen 2 1.1.0.21 (shown in shurectl,
    /// 2026-10-20), MVX2U Gen 2 1.2.0.6 (shown Up to date, 2026-10-20; MOTIV Mix
    /// capture, 2026-10-12), MV7+ 1.3.0.13 (read-only probe, 2026-10-19) and
    /// 1.6.0.1 (tested on hardware, 2026-10-20), MV6 1.3.0.6 (GET probe,
    /// 2026-04-16).
    pub fn status(&self, model: DeviceModel, installed: &str) -> Option<FirmwareStatus<'_>> {
        let latest = self.versions.get(archive_item(model)?)?;
        Some(if parse_firmware(installed)? >= parse_firmware(latest)? {
            FirmwareStatus::UpToDate
        } else {
            FirmwareStatus::UpdateAvailable(latest)
        })
    }
}

/// Ask the archive in the background. The receiver gets one value if the
/// lookup works and nothing if it fails, so the caller polls it with
/// `try_recv()` and never waits on the network.
pub fn spawn_check() -> mpsc::Receiver<LatestFirmware> {
    let (sender, receiver) = mpsc::channel();
    // `Builder::spawn()` returns an error where `thread::spawn()` would panic,
    // and a panic aborts shurectl. Without a thread, the sender is dropped and
    // the receiver never gets a value.
    let _ = thread::Builder::new()
        .name("firmware-check".into())
        .spawn(move || {
            if let Ok(latest) = fetch() {
                // The receiver is gone only when shurectl is quitting.
                let _ = sender.send(latest);
            }
        });
    receiver
}

fn fetch() -> Result<LatestFirmware> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .user_agent(concat!("shurectl/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let body = agent
        .post(ENDPOINT)
        .header("Authorization", format!("Bearer {SEARCH_KEY}"))
        .content_type("application/json")
        .send(request_body().to_string())?
        .body_mut()
        .read_to_string()?;
    parse_response(&body)
}

/// Every firmware release of every compared model, in US English (each
/// language has its own copy of an entry).
fn request_body() -> Value {
    json!({
        "query": "",
        "filters": {"all": [
            {"language": "en-US"},
            {"type": "Firmware"},
            {"itemname": ARCHIVE_ITEMS},
        ]},
        "result_fields": {"itemname": {"raw": {}}, "versionnumber": {"raw": {}}},
        "page": {"size": 100},
    })
}

/// Keep the highest version per item; results come in no particular order.
/// Entries whose version isn't three or four numbers are skipped. A reply
/// spanning more than one page is refused, since the newest release could be on
/// a page not read.
fn parse_response(body: &str) -> Result<LatestFirmware> {
    let response: Value =
        serde_json::from_str(body).context("firmware archive reply isn't JSON")?;
    let pages = response["meta"]["page"]["total_pages"]
        .as_u64()
        .context("firmware archive reply has no page count")?;
    anyhow::ensure!(pages <= 1, "firmware archive reply spans {pages} pages");
    let results = response["results"]
        .as_array()
        .context("firmware archive reply has no results")?;
    let mut latest = LatestFirmware::default();
    for result in results {
        let (Some(item), Some(version)) = (
            result["itemname"]["raw"].as_str(),
            result["versionnumber"]["raw"].as_str(),
        ) else {
            continue;
        };
        let Some(parsed) = parse_firmware(version) else {
            continue;
        };
        let newer = latest
            .versions
            .get(item)
            .and_then(|known| parse_firmware(known))
            .is_none_or(|known| parsed > known);
        if newer {
            latest
                .versions
                .insert(item.to_string(), version.to_string());
        }
    }
    anyhow::ensure!(
        !latest.versions.is_empty(),
        "firmware archive lists no firmware"
    );
    Ok(latest)
}

#[cfg(test)]
pub fn latest_for_tests(versions: &[(&str, &str)]) -> LatestFirmware {
    LatestFirmware {
        versions: versions
            .iter()
            .map(|(item, version)| (item.to_string(), version.to_string()))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODELS: [DeviceModel; 6] = [
        DeviceModel::Mvx2u,
        DeviceModel::Mvx2uGen2,
        DeviceModel::Mv6,
        DeviceModel::Mv6Gen2,
        DeviceModel::Mv7,
        DeviceModel::Mv7Plus,
    ];

    /// The archive's reply to `request_body()`, recorded on 2026-10-20.
    fn recorded() -> LatestFirmware {
        let body = include_str!("../tests/fixtures/firmware-archive.json");
        parse_response(body).expect("the recorded reply parses")
    }

    /// A reply in the archive's shape, with the page count it reports.
    fn reply(results: Value, total_pages: u64) -> String {
        json!({"meta": {"page": {"total_pages": total_pages}}, "results": results}).to_string()
    }

    #[test]
    fn recorded_reply_gives_the_newest_release_of_each_model() {
        let latest = recorded();
        let newest = [
            (DeviceModel::Mvx2uGen2, "1.2.0"),
            (DeviceModel::Mv6, "1.3.0"),
            (DeviceModel::Mv6Gen2, "1.1.0"),
            (DeviceModel::Mv7Plus, "1.6.0"),
        ];
        for (model, version) in newest {
            let item = archive_item(model).expect("compared model");
            assert_eq!(latest.versions[item], version, "{model:?}");
        }
        assert_eq!(latest.versions.len(), newest.len());
    }

    #[test]
    fn request_names_every_compared_model_and_nothing_else() {
        let mut expected: Vec<&str> = MODELS.into_iter().filter_map(archive_item).collect();
        let mut listed = ARCHIVE_ITEMS.to_vec();
        expected.sort_unstable();
        listed.sort_unstable();
        assert_eq!(listed, expected);
        let body = request_body();
        assert_eq!(body["filters"]["all"][2]["itemname"], json!(ARCHIVE_ITEMS));
        assert_eq!(body["filters"]["all"][1]["type"], "Firmware");
    }

    #[test]
    fn status_compares_installed_with_newest() {
        let latest = recorded();
        // Seen on hardware: the MV6 Gen 2 on 1.1.0.21 and the MVX2U Gen 2 on
        // 1.2.0.6 are on the newest release.
        assert_eq!(
            latest.status(DeviceModel::Mv6Gen2, "1.1.0.21"),
            Some(FirmwareStatus::UpToDate)
        );
        assert_eq!(
            latest.status(DeviceModel::Mvx2uGen2, "1.2.0.6"),
            Some(FirmwareStatus::UpToDate)
        );
        assert_eq!(
            latest.status(DeviceModel::Mv7Plus, "1.3.0.13"),
            Some(FirmwareStatus::UpdateAvailable("1.6.0"))
        );
        assert_eq!(
            latest.status(DeviceModel::Mv6, "1.2.0"),
            Some(FirmwareStatus::UpdateAvailable("1.3.0"))
        );
        // Firmware newer than the archive lists is up to date, not an update.
        assert_eq!(
            latest.status(DeviceModel::Mvx2uGen2, "1.3.0.1"),
            Some(FirmwareStatus::UpToDate)
        );
        // A four-part archive version is a newer build of the same release.
        let hotfix = latest_for_tests(&[("mv7plus", "1.6.0.2")]);
        assert_eq!(
            hotfix.status(DeviceModel::Mv7Plus, "1.6.0.1"),
            Some(FirmwareStatus::UpdateAvailable("1.6.0.2"))
        );
    }

    #[test]
    fn status_is_unknown_without_something_to_compare() {
        let latest = recorded();
        assert_eq!(latest.status(DeviceModel::Mv7Plus, "Unknown"), None);
        assert_eq!(latest.status(DeviceModel::Mv7, "0.0.52.0"), None);
        let with_gen1 = latest_for_tests(&[("mvx2u", "1.2.1")]);
        assert_eq!(with_gen1.status(DeviceModel::Mvx2u, "1.2.0.1"), None);
        let only_mv6 = latest_for_tests(&[("MV6", "1.3.0")]);
        assert_eq!(only_mv6.status(DeviceModel::Mv6Gen2, "1.0.0.1"), None);
    }

    #[test]
    fn only_compared_models_are_looked_up() {
        for model in MODELS {
            let expected = !matches!(model, DeviceModel::Mvx2u | DeviceModel::Mv7);
            assert_eq!(is_compared(model), expected, "{model:?}");
        }
    }

    #[test]
    fn newest_version_wins_whatever_the_order() {
        let body = reply(
            json!([
                {"itemname": {"raw": "mv7plus"}, "versionnumber": {"raw": "1.10.0"}},
                {"itemname": {"raw": "mv7plus"}, "versionnumber": {"raw": "1.9.0"}},
                {"itemname": {"raw": "mv7plus"}, "versionnumber": {"raw": "beta"}},
                {"itemname": {"raw": "MV6"}},
            ]),
            1,
        );
        let latest = parse_response(&body).expect("parses");
        assert_eq!(latest.versions["mv7plus"], "1.10.0");
        assert!(!latest.versions.contains_key("MV6"));
    }

    #[test]
    fn unusable_replies_are_errors() {
        let release = json!([{"itemname": {"raw": "mv7plus"}, "versionnumber": {"raw": "1.6.0"}}]);
        // The archive's replies to a wrong key and a wrong engine (HTTP 401 and
        // 404, 2026-10-20), and other shapes it could change to.
        for body in [
            String::new(),
            "<html>Moved</html>".into(),
            r#"{"error":"Invalid credentials"}"#.into(),
            r#"{"errors":["Could not find engine."]}"#.into(),
            json!({"results": release}).to_string(),
            reply(release.clone(), 2),
            reply(json!({}), 1),
            reply(json!([]), 0),
            reply(
                json!([{"title": {"raw": "MV7+"}, "version": {"raw": "1.6.0"}}]),
                1,
            ),
        ] {
            assert!(parse_response(&body).is_err(), "{body}");
        }
        assert!(parse_response(&reply(release, 1)).is_ok());
    }

    /// Asks the real archive: `cargo test -- --ignored`. Run it when the Info tab
    /// stops showing the firmware status; a failure means the endpoint, key or
    /// fields changed, and the archive page's JavaScript has the new ones.
    #[test]
    #[ignore = "needs the network"]
    fn live_archive_lists_every_compared_model() {
        let latest = fetch().expect("the archive answers");
        for item in ARCHIVE_ITEMS {
            assert!(latest.versions.contains_key(item), "{item} missing");
        }
    }
}
