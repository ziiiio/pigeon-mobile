//! Key-directory orchestration (M3.2): discover a user's devices and claim a
//! KeyPackage from each, so we can add them to an MLS group.
//!
//! The HTTP verbs live in [`crate::api`] (`/keys/query`, `/keys/claim`); this is
//! the *sequencing* the reference CLI's `invite` performs — query the user's
//! devices, then claim one KeyPackage per device — factored out so the
//! invite-with-Welcome flow (M3.4) reuses it. Claiming consumes a one-time
//! KeyPackage per device (or the reusable last-resort one); a device with none
//! left to give is skipped, exactly as the CLI does.

use crate::api::Api;
use crate::CoreError;

/// A KeyPackage claimed for one of a user's devices — the input to
/// `Device::add_member` when building that device's `Welcome` (M3.4).
#[derive(Debug, Clone)]
pub struct ClaimedKeyPackage {
    /// The device the package was claimed from (a Welcome is per-device).
    pub device_id: String,
    /// The base64 MLS KeyPackage.
    pub key_package_b64: String,
}

/// Claim a KeyPackage from every published device of `user_id`.
///
/// Queries the user's device list (`/keys/query`) then claims one package per
/// device (`/keys/claim`). Devices that have exhausted their pool (and published
/// no last-resort package) are silently skipped. Returns an empty vec if the user
/// has published no keys at all (their side hasn't set up E2EE) — the caller
/// decides whether that's an error for the flow.
pub async fn claim_all_devices(
    api: &Api,
    user_id: &str,
) -> Result<Vec<ClaimedKeyPackage>, CoreError> {
    let devices = api.query_keys(user_id).await?;
    // The query response for a user is a `{ device_id -> DeviceKeys }` map; its
    // keys are the device ids to claim from. Absent/empty ⇒ nothing published.
    let device_ids: Vec<String> = devices
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();

    let mut claimed = Vec::with_capacity(device_ids.len());
    for device_id in device_ids {
        if let Some(key_package_b64) = api.claim_keys(user_id, &device_id).await? {
            claimed.push(ClaimedKeyPackage {
                device_id,
                key_package_b64,
            });
        }
    }
    Ok(claimed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn claims_a_key_package_from_each_published_device() {
        let server = MockServer::start().await;
        // Two devices published for the user.
        Mock::given(method("POST"))
            .and(path("/_pigeon/client/v1/keys/query"))
            .and(body_json(
                json!({ "device_keys": { "@bob:test.example": [] } }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_keys": {
                    "@bob:test.example": {
                        "DEV1": { "user_id": "@bob:test.example", "device_id": "DEV1",
                                  "algorithms": ["p.mls.1"], "keys": {}, "signatures": {} },
                        "DEV2": { "user_id": "@bob:test.example", "device_id": "DEV2",
                                  "algorithms": ["p.mls.1"], "keys": {}, "signatures": {} }
                    }
                }
            })))
            .mount(&server)
            .await;
        // DEV1 hands out a package; DEV2 has none left (omitted from the response).
        Mock::given(method("POST"))
            .and(path("/_pigeon/client/v1/keys/claim"))
            .and(body_json(json!({ "one_time_keys": { "@bob:test.example": ["DEV1"] } })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "one_time_keys": { "@bob:test.example": { "DEV1": { "key_id": "kp-0", "package": "AAAA" } } }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/_pigeon/client/v1/keys/claim"))
            .and(body_json(
                json!({ "one_time_keys": { "@bob:test.example": ["DEV2"] } }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "one_time_keys": { "@bob:test.example": {} }
            })))
            .mount(&server)
            .await;

        let api = Api::new(server.uri(), Some("tok".into())).unwrap();
        let claimed = claim_all_devices(&api, "@bob:test.example").await.unwrap();

        // Only the device that had a package to give is returned.
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].device_id, "DEV1");
        assert_eq!(claimed[0].key_package_b64, "AAAA");
    }

    #[tokio::test]
    async fn no_published_keys_yields_empty() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/_pigeon/client/v1/keys/query"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "device_keys": {} })))
            .mount(&server)
            .await;

        let api = Api::new(server.uri(), Some("tok".into())).unwrap();
        let claimed = claim_all_devices(&api, "@nobody:test.example")
            .await
            .unwrap();
        assert!(claimed.is_empty());
    }
}
