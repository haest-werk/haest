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

use haest_http::{HttpClient, Request};
use serde::Deserialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{
    request::{ImageError, exchange},
    settings::{ModelSettings, SettingSpec},
};

const FRESH: Duration = Duration::from_secs(60 * 60);

type Models = Arc<HashMap<String, ModelSettings>>;

/// The Image API's model lists read so far, by base URL, each kept an hour.
/// [`GenerateImageOptions`](super::GenerateImageOptions) holds one; `Default` is empty.
#[derive(Debug, Default)]
pub(crate) struct ImageCatalog {
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

/// The list at `base_url`; None when it could not be read or names no model
/// (the caller falls back to its last good copy, or goes unchecked).
async fn fetch(
    base_url: &str,
    http: &dyn HttpClient,
    cancel: &CancellationToken,
) -> Option<Models> {
    let get = Request::get(format!("{base_url}/images/models"));
    let (status, body) = exchange(http, get, cancel).await.ok()?;
    if !(200..300).contains(&status) {
        return None;
    }
    let list: Value = serde_json::from_slice(&body).ok()?;
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
    (!models.is_empty()).then(|| Arc::new(models))
}

impl ImageCatalog {
    /// Every listed model's settings, at most an hour old; None when the list was never read.
    async fn all(
        &self,
        base_url: &str,
        http: &dyn HttpClient,
        cancel: &CancellationToken,
    ) -> Option<Models> {
        let kept = self.kept.lock().unwrap().get(base_url).cloned();
        if let Some((at, models)) = &kept
            && at.elapsed() < FRESH
        {
            return Some(models.clone());
        }
        match fetch(base_url, http, cancel).await {
            Some(models) => {
                let entry = (Instant::now(), models.clone());
                self.kept.lock().unwrap().insert(base_url.into(), entry);
                Some(models)
            }
            None => kept.map(|(_, models)| models),
        }
    }

    /// What `model` takes, from the list at `base_url` (read through `http`
    /// when there is no copy under an hour old); None when the list could not
    /// be read and no copy is kept.
    ///
    /// # Errors
    ///
    /// [`ImageError::UnknownModel`] when the list was read and `model` is not
    /// on it.
    pub(crate) async fn model_params(
        &self,
        http: &dyn HttpClient,
        base_url: &str,
        model: &str,
        cancel: &CancellationToken,
    ) -> Result<Option<ModelSettings>, ImageError> {
        let Some(all) = self.all(base_url, http, cancel).await else {
            return Ok(None);
        };
        match all.get(model) {
            Some(params) => Ok(Some(params.clone())),
            None => Err(ImageError::UnknownModel {
                model: model.into(),
                base_url: base_url.into(),
            }),
        }
    }
}
