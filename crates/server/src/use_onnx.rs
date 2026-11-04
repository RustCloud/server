//! `use-onnx` for plugins: the neural network engine ([`rustcloud_ml`])
//! behind the WIT import, with models from the plugins' storage and inputs
//! made by the media engine, and its state on the admin page.

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use rustcloud_auth::AdminUser;
use rustcloud_core::{Error, Result};
use rustcloud_media::{Crop, Media, PixelFit, PixelOptions, SampleOptions};
use rustcloud_ml::{Channels, DType, LoadedModel, ModelFile, NamedTensor, Normalize, Onnx, Priority, Status, Tensor};
use rustcloud_wasm_host::{onnx, AppStorage, UseOnnx};
use serde::Serialize;

use crate::plugin_storage::PluginStorage;
use crate::AppState;

/// The engine as the WASM host sees it.
pub struct OnnxTools {
    pub onnx: Arc<Onnx>,
    pub media: Arc<Media>,
    pub storage: Arc<PluginStorage>,
}

impl OnnxTools {
    /// The model file `path` of an app's storage, by its current content.
    async fn model(&self, app_id: &str, path: &str) -> Result<ModelFile> {
        let stored = self.storage.stat(app_id, path).await?;
        Ok(ModelFile { app_id: app_id.to_owned(), path: stored.path, sha256: stored.sha256 })
    }
}

#[async_trait]
impl UseOnnx for OnnxTools {
    fn capabilities(&self) -> onnx::Capabilities {
        let status = self.onnx.status();
        onnx::Capabilities {
            available: status.available,
            engine: status.engine.clone(),
            keep_loaded_seconds: self.onnx.keep_loaded_seconds(),
        }
    }

    fn forget(&self, app_id: &str) {
        self.onnx.forget(app_id);
    }

    async fn model_info(&self, app_id: &str, model: &str, background: bool) -> Result<onnx::ModelInfo> {
        let file = self.model(app_id, model).await?;
        let info = self.onnx.info(&file, || self.storage.open(app_id, model), priority(background)).await?;
        let describe = |list: Vec<rustcloud_ml::TensorInfo>| {
            list.into_iter()
                .map(|t| onnx::TensorInfo { name: t.name, dtype: t.dtype.map(to_wit), shape: t.shape })
                .collect()
        };
        Ok(onnx::ModelInfo { inputs: describe(info.inputs), outputs: describe(info.outputs) })
    }

    async fn run(
        &self,
        app_id: &str,
        model: &str,
        inputs: Vec<onnx::NamedTensor>,
        limit: u64,
        background: bool,
    ) -> Result<Vec<onnx::NamedTensor>> {
        let file = self.model(app_id, model).await?;
        let inputs = inputs.into_iter().map(|t| NamedTensor { name: t.name, tensor: from_wit(t.tensor) }).collect();
        let open = || self.storage.open(app_id, model);
        let outputs = self.onnx.run(&file, open, inputs, limit, priority(background)).await?;
        Ok(outputs.into_iter().map(|t| onnx::NamedTensor { name: t.name, tensor: tensor_to_wit(t.tensor) }).collect())
    }

    async fn image_tensor(
        &self,
        file: std::fs::File,
        options: onnx::ImageOptions,
        limit: u64,
        background: bool,
    ) -> Result<onnx::ImageInput> {
        // Refused before the work: f32, three per pixel.
        let size = u64::from(options.width) * u64::from(options.height) * 3 * 4;
        if size > limit {
            return Err(Error::bad_request("ONNX_FAILED", "the tensor is larger than a call takes at once"));
        }
        let pixels = PixelOptions {
            width: options.width,
            height: options.height,
            fit: match options.fit {
                onnx::ImageFit::Stretch => PixelFit::Stretch,
                onnx::ImageFit::Inside => PixelFit::Inside,
                onnx::ImageFit::Cover => PixelFit::Cover,
            },
            crop: options.crop.map(|c| Crop { x: c.x, y: c.y, width: c.width, height: c.height }),
        };
        let picture = self.media.pixels(file, &pixels, media_priority(background)).await?;
        let normalize = Normalize {
            channels: match options.channels {
                onnx::Channels::Rgb => Channels::Rgb,
                onnx::Channels::Bgr => Channels::Bgr,
            },
            layout: match options.layout {
                onnx::Layout::Nchw => rustcloud_ml::Layout::Nchw,
                onnx::Layout::Nhwc => rustcloud_ml::Layout::Nhwc,
            },
            mean: [options.mean.0, options.mean.1, options.mean.2],
            std: [options.std.0, options.std.1, options.std.2],
        };
        let tensor = rustcloud_ml::image_tensor(&picture.rgb, options.width, options.height, &normalize)?;
        Ok(onnx::ImageInput { tensor: tensor_to_wit(tensor), width: picture.width, height: picture.height })
    }

    async fn audio_tensor(
        &self,
        file: std::fs::File,
        options: onnx::AudioOptions,
        limit: u64,
        background: bool,
    ) -> Result<onnx::Tensor> {
        let options =
            SampleOptions { rate: options.rate, start_ms: options.start_ms, duration_ms: options.duration_ms };
        let data = self.media.samples(file, &options, limit, media_priority(background)).await?;
        let samples = u32::try_from(data.len() / 4).map_err(Error::internal)?;
        Ok(onnx::Tensor { dtype: onnx::TensorType::F32, shape: vec![samples], data })
    }
}

fn priority(background: bool) -> Priority {
    if background {
        Priority::Background
    } else {
        Priority::Request
    }
}

fn media_priority(background: bool) -> rustcloud_media::Priority {
    if background {
        rustcloud_media::Priority::Background
    } else {
        rustcloud_media::Priority::Request
    }
}

fn to_wit(dtype: DType) -> onnx::TensorType {
    match dtype {
        DType::F32 => onnx::TensorType::F32,
        DType::F64 => onnx::TensorType::F64,
        DType::I32 => onnx::TensorType::I32,
        DType::I64 => onnx::TensorType::I64,
        DType::I8 => onnx::TensorType::I8,
        DType::U8 => onnx::TensorType::U8,
        DType::Bool => onnx::TensorType::Bool,
    }
}

fn from_wit(tensor: onnx::Tensor) -> Tensor {
    let dtype = match tensor.dtype {
        onnx::TensorType::F32 => DType::F32,
        onnx::TensorType::F64 => DType::F64,
        onnx::TensorType::I32 => DType::I32,
        onnx::TensorType::I64 => DType::I64,
        onnx::TensorType::I8 => DType::I8,
        onnx::TensorType::U8 => DType::U8,
        onnx::TensorType::Bool => DType::Bool,
    };
    Tensor { dtype, shape: tensor.shape, data: tensor.data }
}

fn tensor_to_wit(tensor: Tensor) -> onnx::Tensor {
    onnx::Tensor { dtype: to_wit(tensor.dtype), shape: tensor.shape, data: tensor.data }
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/admin/use/onnx", get(status))
}

/// The state of `use.onnx` on this server.
#[derive(Serialize)]
struct OnnxStatus {
    #[serde(flatten)]
    status: Status,
    keep_loaded_seconds: u32,
    /// The models loaded now, the most recently used first.
    loaded: Vec<LoadedModel>,
}

/// `GET /api/v1/admin/use/onnx`: whether the engine runs, what confines it
/// and which models are loaded.
async fn status(State(onnx): State<Arc<Onnx>>, _admin: AdminUser) -> Json<OnnxStatus> {
    Json(OnnxStatus {
        status: onnx.status().clone(),
        keep_loaded_seconds: onnx.keep_loaded_seconds(),
        loaded: onnx.loaded(),
    })
}
