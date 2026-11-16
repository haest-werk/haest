//! The Image API's model list (`GET /images/models`: 63 models on 2026-11-05),
//! each with the settings it takes. Read on the first GenerateImage call and
//! kept an hour; a failed refresh keeps serving the last good copy, and with
//! none at all the call goes ahead unchecked (no settings to check against).
//! Ported from the TS haest's `tools/image-catalog.ts`; the copy is held by the
//! tool's options (shared by their clones), not by the process.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde::Deserialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{
    request::ImageHttp,
    settings::{ModelSettings, SettingSpec},
};

const FRESH: Duration = Duration::from_secs(60 * 60);

type Models = Arc<HashMap<String, ModelSettings>>;

/// The Image API's model lists read so far, by base URL, each kept an hour.
/// [`ImageToolOptions`](super::ImageToolOptions) holds one; `Default` is empty.
#[derive(Default)]
pub struct ImageCatalog {
    kept: Mutex<HashMap<String, (Instant, Models)>>,
}

/// One model's settings; a setting whose spec is not an object is taken as "takes it".
fn settings_of(params: &serde_json::Map<String, Value>) -> ModelSettings {
    params
        .iter()
        .map(|(name, spec)| {
            (
                name.clone(),
                SettingSpec::deserialize(spec).unwrap_or_default(),
            )
        })
        .collect()
}

async fn fetch(
    base_url: &str,
    http: &dyn ImageHttp,
    cancel: CancellationToken,
) -> Result<Models, String> {
    let res = http
        .get(&format!("{base_url}/images/models"), cancel)
        .await?;
    if !res.ok() {
        return Err(format!("the image model list answered HTTP {}", res.status));
    }
    let list: Value = serde_json::from_slice(&res.body).map_err(|e| e.to_string())?;
    let models: HashMap<String, ModelSettings> = list["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["id"].as_str().filter(|id| !id.is_empty())?;
            Some((
                id.to_string(),
                settings_of(m["supported_parameters"].as_object()?),
            ))
        })
        .collect();
    if models.is_empty() {
        return Err("the image model list was empty".into());
    }
    Ok(Arc::new(models))
}

impl ImageCatalog {
    /// Every listed model's settings, at most an hour old; None when the list was never read.
    async fn all(
        &self,
        base_url: &str,
        http: &dyn ImageHttp,
        cancel: CancellationToken,
    ) -> Option<Models> {
        let kept = self.kept.lock().unwrap().get(base_url).cloned();
        if let Some((at, models)) = &kept
            && at.elapsed() < FRESH
        {
            return Some(models.clone());
        }
        match fetch(base_url, http, cancel).await {
            Ok(models) => {
                let entry = (Instant::now(), models.clone());
                self.kept.lock().unwrap().insert(base_url.into(), entry);
                Some(models)
            }
            Err(_) => kept.map(|(_, models)| models),
        }
    }

    /// What `model` takes, from the list at `base_url` (read through `http`
    /// when there is no copy under an hour old); None when the list could not
    /// be read and no copy is kept.
    ///
    /// # Errors
    ///
    /// When the list was read and `model` is not on it.
    pub async fn model_params(
        &self,
        base_url: &str,
        model: &str,
        http: &dyn ImageHttp,
        cancel: CancellationToken,
    ) -> Result<Option<ModelSettings>, String> {
        let Some(all) = self.all(base_url, http, cancel).await else {
            return Ok(None);
        };
        match all.get(model) {
            Some(params) => Ok(Some(params.clone())),
            None => Err(format!(
                "{model} is not on the Image API's model list (GET {base_url}/images/models)"
            )),
        }
    }
}
