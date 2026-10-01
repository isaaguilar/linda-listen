use crate::{
    config::AppConfig,
    error::{AppError, AppResult},
};
use directories::ProjectDirs;
use reqwest::Client;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

const MODEL_CACHE_DIR: &str = "parakeet-tdt-0.6b-v3-int8";
const MODEL_ASSETS: &[ModelAsset] = &[
    ModelAsset {
        filename: "encoder-model.int8.onnx",
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/encoder-model.int8.onnx",
    },
    ModelAsset {
        filename: "decoder_joint-model.int8.onnx",
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/decoder_joint-model.int8.onnx",
    },
    ModelAsset {
        filename: "vocab.txt",
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/vocab.txt",
    },
];

#[derive(Debug, Clone, Copy)]
struct ModelAsset {
    filename: &'static str,
    url: &'static str,
}

#[derive(Debug, Clone)]
pub struct ParakeetModelManager {
    default_model_dir: PathBuf,
}

impl ParakeetModelManager {
    pub fn load() -> AppResult<Self> {
        let dirs = ProjectDirs::from("com", "galleybytes", "linda-listen")
            .ok_or(AppError::ConfigDirUnavailable)?;
        let default_model_dir = dirs.config_dir().join("models").join(MODEL_CACHE_DIR);

        Ok(Self { default_model_dir })
    }

    pub fn resolved_model_dir(&self, config: &AppConfig) -> PathBuf {
        let custom = config.whisper_model_path.trim();
        if custom.is_empty() {
            self.default_model_dir.clone()
        } else {
            PathBuf::from(custom)
        }
    }

    pub async fn ensure_ready(&self, client: &Client, config: &AppConfig) -> AppResult<PathBuf> {
        let model_dir = self.resolved_model_dir(config);
        tokio::fs::create_dir_all(&model_dir).await?;

        for asset in MODEL_ASSETS {
            self.ensure_asset(client, &model_dir, asset).await?;
        }

        Ok(model_dir)
    }

    pub async fn is_ready(&self, config: &AppConfig) -> AppResult<bool> {
        let model_dir = self.resolved_model_dir(config);

        for asset in MODEL_ASSETS {
            if !Self::asset_exists(&model_dir.join(asset.filename)).await? {
                return Ok(false);
            }
        }

        Ok(true)
    }

    async fn ensure_asset(
        &self,
        client: &Client,
        model_dir: &Path,
        asset: &ModelAsset,
    ) -> AppResult<()> {
        let target_path = model_dir.join(asset.filename);
        if Self::asset_exists(&target_path).await? {
            return Ok(());
        }

        let temp_path = model_dir.join(format!("{}.download", asset.filename));
        if tokio::fs::try_exists(&temp_path).await.unwrap_or(false) {
            let _ = tokio::fs::remove_file(&temp_path).await;
        }

        let response = client.get(asset.url).send().await?.error_for_status()?;
        let mut file = tokio::fs::File::create(&temp_path).await?;
        let mut response = response;

        while let Some(chunk) = response.chunk().await? {
            file.write_all(&chunk).await?;
        }

        file.flush().await?;
        tokio::fs::rename(&temp_path, &target_path).await?;
        Ok(())
    }

    async fn asset_exists(path: &Path) -> AppResult<bool> {
        match tokio::fs::metadata(path).await {
            Ok(metadata) => Ok(metadata.is_file() && metadata.len() > 0),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_default_model_dir_into_app_storage() {
        let manager = ParakeetModelManager::load().unwrap();
        let config = AppConfig::default();
        let path = manager.resolved_model_dir(&config);

        assert!(path.to_string_lossy().contains(MODEL_CACHE_DIR));
    }

    #[test]
    fn uses_custom_model_dir_when_configured() {
        let manager = ParakeetModelManager::load().unwrap();
        let mut config = AppConfig::default();
        config.whisper_model_path = "/tmp/custom-parakeet-model".to_owned();

        assert_eq!(
            manager.resolved_model_dir(&config),
            PathBuf::from("/tmp/custom-parakeet-model")
        );
    }
}
