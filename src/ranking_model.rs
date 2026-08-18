use fuzzy_rank::fields::FieldRankModel;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct StoredFieldRankModel {
    schema_version: u32,
    bias: f64,
    weights: Vec<f64>,
}

pub(crate) fn load_field_rank_model() -> Option<FieldRankModel> {
    let path = std::env::var_os("APPLICATIONLAUNCHER_FIELD_RANK_MODEL")
        .map(PathBuf::from)
        .or_else(default_model_path)?;
    let bytes = std::fs::read(&path).ok()?;
    let stored = serde_json::from_slice::<StoredFieldRankModel>(&bytes).ok()?;

    let Some(model) =
        FieldRankModel::from_versioned_parts(stored.schema_version, stored.bias, &stored.weights)
    else {
        eprintln!(
            "Ignoring incompatible application field-rank model: {}",
            path.display()
        );
        return None;
    };
    model.is_active().then_some(model)
}

fn default_model_path() -> Option<PathBuf> {
    let state_dir = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(
        state_dir
            .join("applicationlauncher")
            .join("field-rank-model.json"),
    )
}
