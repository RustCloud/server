//! Tensors as plugins send and get them: a type, a shape, and the values as
//! little-endian bytes, row by row (the last index changes fastest).

use rustcloud_core::{Error, Result};
use serde::{Deserialize, Serialize};

/// The types of values a tensor may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DType {
    F32,
    F64,
    I32,
    I64,
    I8,
    U8,
    /// One byte, 0 or 1.
    Bool,
}

impl DType {
    /// As plugins write it: `f32`, `i64`…
    pub fn name(self) -> &'static str {
        match self {
            DType::F32 => "f32",
            DType::F64 => "f64",
            DType::I32 => "i32",
            DType::I64 => "i64",
            DType::I8 => "i8",
            DType::U8 => "u8",
            DType::Bool => "bool",
        }
    }

    /// Bytes of one value.
    pub fn size(self) -> usize {
        match self {
            DType::F64 | DType::I64 => 8,
            DType::F32 | DType::I32 => 4,
            DType::I8 | DType::U8 | DType::Bool => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tensor {
    pub dtype: DType,
    pub shape: Vec<u32>,
    pub data: Vec<u8>,
}

impl Tensor {
    /// Bytes the shape needs; none if it overflows.
    pub fn expected_size(dtype: DType, shape: &[u32]) -> Option<u64> {
        shape.iter().try_fold(dtype.size() as u64, |size, &dim| size.checked_mul(u64::from(dim)))
    }

    /// Whether the data fits the type and shape.
    pub fn check(&self, name: &str) -> Result<()> {
        if Tensor::expected_size(self.dtype, &self.shape) != Some(self.data.len() as u64) {
            return Err(invalid(format!(
                "{name}: {} bytes don't make a {} tensor of shape {:?}",
                self.data.len(),
                self.dtype.name(),
                self.shape
            )));
        }
        if self.dtype == DType::Bool && self.data.iter().any(|&byte| byte > 1) {
            return Err(invalid(format!("{name}: bool values must be 0 or 1")));
        }
        Ok(())
    }

    /// Little-endian 32-bit floats, shaped.
    pub fn from_f32(shape: Vec<u32>, values: &[f32]) -> Self {
        Tensor { dtype: DType::F32, shape, data: values.iter().flat_map(|v| v.to_le_bytes()).collect() }
    }
}

/// A tensor with the name of the model's input or output it is for.
#[derive(Debug, Clone, PartialEq)]
pub struct NamedTensor {
    pub name: String,
    pub tensor: Tensor,
}

/// An input or output of a model, as the model declares it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TensorInfo {
    pub name: String,
    /// None: a type plugins can't send or get (f16, strings…).
    pub dtype: Option<DType>,
    /// None for a dimension the model leaves open (a batch, a text's
    /// length); none at all if not even their number is known before a run.
    pub shape: Option<Vec<Option<u32>>>,
}

/// What a model takes and gives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub inputs: Vec<TensorInfo>,
    pub outputs: Vec<TensorInfo>,
}

/// Which order a picture's colours go in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channels {
    Rgb,
    Bgr,
}

/// Where the colour goes in a picture's tensor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// `[1, 3, height, width]`: a plane per colour (most models).
    Nchw,
    /// `[1, height, width, 3]`: the colours of each pixel together.
    Nhwc,
}

/// How pixels become a model's input: each value `v` (0–255) of colour `c`
/// becomes `(v - mean[c]) / std[c]`, `c` in the order of `channels`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Normalize {
    pub channels: Channels,
    pub layout: Layout,
    pub mean: [f32; 3],
    pub std: [f32; 3],
}

/// The f32 tensor of RGB pixels (three bytes each, row by row) of a picture
/// `width`×`height`.
pub fn image_tensor(rgb: &[u8], width: u32, height: u32, normalize: &Normalize) -> Result<Tensor> {
    if normalize.std.iter().any(|&std| std == 0.0 || !std.is_finite()) || normalize.mean.iter().any(|m| !m.is_finite())
    {
        return Err(invalid("std must not be 0, and mean and std must be numbers".to_owned()));
    }
    let pixels = width as usize * height as usize;
    if rgb.len() != pixels * 3 {
        return Err(Error::internal("the pixels are not of the picture's size"));
    }
    // Which byte of a pixel each colour of the output takes.
    let order = match normalize.channels {
        Channels::Rgb => [0, 1, 2],
        Channels::Bgr => [2, 1, 0],
    };
    let value = |pixel: usize, c: usize| (f32::from(rgb[pixel * 3 + order[c]]) - normalize.mean[c]) / normalize.std[c];
    let (values, shape): (Vec<f32>, _) = match normalize.layout {
        Layout::Nchw => {
            ((0..3).flat_map(|c| (0..pixels).map(move |p| value(p, c))).collect(), vec![1, 3, height, width])
        }
        Layout::Nhwc => {
            ((0..pixels).flat_map(|p| (0..3).map(move |c| value(p, c))).collect(), vec![1, height, width, 3])
        }
    };
    Ok(Tensor::from_f32(shape, &values))
}

pub(crate) fn invalid(message: String) -> Error {
    Error::bad_request("ONNX_INVALID_INPUT", message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floats(tensor: &Tensor) -> Vec<f32> {
        tensor.data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
    }

    #[test]
    fn data_must_fit_type_and_shape() {
        let ok = Tensor { dtype: DType::I64, shape: vec![2, 3], data: vec![0; 48] };
        assert!(ok.check("x").is_ok());
        assert!(Tensor { data: vec![0; 47], ..ok.clone() }.check("x").is_err());
        assert!(Tensor { shape: vec![u32::MAX, u32::MAX, u32::MAX], ..ok }.check("x").is_err(), "overflow");
        assert!(Tensor { dtype: DType::Bool, shape: vec![2], data: vec![1, 2] }.check("x").is_err());
    }

    #[test]
    fn pixels_become_planes_or_pixels_normalized() {
        // Two pixels: red, then blue.
        let rgb = [255, 0, 0, 0, 0, 255];
        let plain = Normalize { channels: Channels::Rgb, layout: Layout::Nchw, mean: [0.0; 3], std: [255.0; 3] };
        let planes = image_tensor(&rgb, 2, 1, &plain).unwrap();
        assert_eq!(planes.shape, [1, 3, 1, 2]);
        assert_eq!(floats(&planes), [1.0, 0.0, 0.0, 0.0, 0.0, 1.0]);

        let bgr = Normalize { channels: Channels::Bgr, layout: Layout::Nhwc, mean: [127.5; 3], std: [127.5; 3] };
        let pixels = image_tensor(&rgb, 2, 1, &bgr).unwrap();
        assert_eq!(pixels.shape, [1, 1, 2, 3]);
        assert_eq!(floats(&pixels), [-1.0, -1.0, 1.0, 1.0, -1.0, -1.0]);

        assert!(image_tensor(&rgb, 2, 1, &Normalize { std: [0.0; 3], ..plain }).is_err());
    }
}
