//! `rustcloud onnx-worker`: a process that holds one model and runs it.
//!
//! The server starts one per loaded model. The worker confines itself
//! ([`rustcloud_sandbox::confine_self`]) before it reads a byte, so a model
//! or an input made to break tract finds a process that can't open a file,
//! reach the network or start a program. Then it speaks [`crate::protocol`]
//! on its stdin and stdout until the server closes them.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufReader, BufWriter, Write};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use tract_onnx::prelude::*;
use tract_onnx::tract_hir::infer::{DimFact, GenericFactoid, TypeFactoid};

use crate::protocol::{self, Header, Reply, Run};
use crate::tensor::{DType, ModelInfo, TensorInfo};

/// `rustcloud onnx-worker --memory <bytes>`, or `--report` to confine itself
/// and print the layers that took, as JSON. `args` are what follows
/// `onnx-worker`.
pub fn main(args: Vec<OsString>) -> ! {
    match run(args) {
        Ok(()) => std::process::exit(0),
        Err(message) => {
            eprintln!("rustcloud onnx-worker: {message}");
            std::process::exit(1)
        }
    }
}

fn run(args: Vec<OsString>) -> Result<(), String> {
    let mut memory: u64 = 2048 * 1024 * 1024;
    let mut report = false;
    let mut args = args.into_iter().map(|arg| arg.into_string().unwrap_or_default());
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--memory" => {
                let value = args.next().unwrap_or_default();
                memory = value.parse().map_err(|_| format!("--memory: \"{value}\" is not a number"))?;
            }
            "--report" => report = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let layers = rustcloud_sandbox::confine_self(memory)?;
    if report {
        println!("{}", serde_json::to_string(&layers).map_err(|err| err.to_string())?);
        return Ok(());
    }

    let io = |err: std::io::Error| err.to_string();
    let mut input = BufReader::new(std::io::stdin().lock());
    let mut output = BufWriter::new(std::io::stdout().lock());
    let Some(bytes) = protocol::read_frame(&mut input, u64::MAX).map_err(io)? else {
        return Ok(());
    };
    let mut model = match catch(|| Model::load(&bytes)) {
        Ok(model) => model,
        Err(why) => {
            protocol::write_message(&mut output, &Reply::Failed(why.clone())).map_err(io)?;
            output.flush().map_err(io)?;
            return Err(why);
        }
    };
    drop(bytes);
    protocol::write_message(&mut output, &Reply::Loaded(model.info.clone())).map_err(io)?;
    output.flush().map_err(io)?;

    while let Some(frame) = protocol::read_frame(&mut input, protocol::MAX_MESSAGE).map_err(io)? {
        let run: Run = serde_json::from_slice(&frame).map_err(|err| err.to_string())?;
        let mut data = Vec::with_capacity(run.inputs.len());
        for _ in &run.inputs {
            data.push(protocol::read_frame(&mut input, u64::MAX).map_err(io)?.ok_or("the input was cut short")?);
        }
        match catch(|| model.run(&run, &data)) {
            Ok(outputs) => {
                let headers = outputs.iter().map(|(header, _)| header.clone()).collect();
                protocol::write_message(&mut output, &Reply::Outputs(headers)).map_err(io)?;
                for (_, data) in &outputs {
                    protocol::write_frame(&mut output, data).map_err(io)?;
                }
            }
            Err(why) => protocol::write_message(&mut output, &Reply::Failed(why)).map_err(io)?,
        }
        output.flush().map_err(io)?;
    }
    Ok(())
}

/// Runs `work`, turning a panic in tract into an error: one bad input must
/// not unload the model.
fn catch<T>(work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(AssertUnwindSafe(work)).unwrap_or_else(|panic| {
        let why = panic
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "the engine failed".to_owned());
        Err(why)
    })
}

/// The most plans for particular input shapes a worker keeps.
const SHAPE_PLANS: usize = 8;

/// A loaded model, ready to run.
///
/// Many models leave their inputs' shapes open or declare them so that tract
/// can't plan for every shape (a face detector takes any picture size, a
/// voice model says nothing at all). Those are planned for the shapes of each
/// run instead, once per shape: a batch of runs of one size plans once.
struct Model {
    /// As read, for plans of particular input shapes.
    source: InferenceModel,
    /// One plan for any input shape, if the model's own declarations allow.
    general: Option<Arc<TypedRunnableModel>>,
    /// Plans for the input shapes seen, if not.
    by_shape: HashMap<Vec<Header>, Arc<TypedRunnableModel>>,
    /// What the model declares of its inputs, in its order.
    declared: Vec<Declared>,
    info: ModelInfo,
}

/// An input as the model declares it; each part may be unknown.
struct Declared {
    name: String,
    datum: Option<DatumType>,
    /// None: not even the number of dimensions is known.
    shape: Option<Vec<Option<u32>>>,
}

fn failed(err: impl std::fmt::Display) -> String {
    // tract's errors carry their causes; the first lines are enough.
    format!("{err:#}").chars().take(500).collect()
}

impl Model {
    fn load(bytes: &[u8]) -> Result<Self, String> {
        let source = tract_onnx::onnx().model_for_read(&mut &bytes[..]).map_err(failed)?;
        let declared = (0..source.inputs.len())
            .map(|index| {
                let outlet = source.inputs[index];
                let fact = source.input_fact(index).map_err(failed)?;
                Ok(Declared {
                    name: source.node(outlet.node).name.clone(),
                    datum: concrete(&fact.datum_type),
                    shape: (!fact.shape.is_open()).then(|| fact.shape.dims().map(dim).collect()),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let general = source.clone().into_optimized().and_then(|model| model.into_runnable()).ok();
        let info = match &general {
            // The plan knows its outputs, and its inputs better than the file.
            Some(plan) => {
                let model = plan.model();
                let describe = |outlets: &[OutletId]| -> Result<Vec<TensorInfo>, String> {
                    outlets.iter().map(|&outlet| typed_info(model, outlet).map_err(failed)).collect()
                };
                ModelInfo {
                    inputs: describe(model.input_outlets().map_err(failed)?)?,
                    outputs: describe(model.output_outlets().map_err(failed)?)?,
                }
            }
            None => ModelInfo {
                inputs: declared
                    .iter()
                    .map(|input| TensorInfo {
                        name: input.name.clone(),
                        dtype: input.datum.and_then(dtype),
                        shape: input.shape.clone(),
                    })
                    .collect(),
                outputs: source
                    .outputs
                    .iter()
                    .map(|&outlet| {
                        let fact = source.outlet_fact(outlet).map_err(failed)?;
                        Ok(TensorInfo {
                            name: label(&source, outlet),
                            dtype: concrete(&fact.datum_type).and_then(dtype),
                            shape: (!fact.shape.is_open()).then(|| fact.shape.dims().map(dim).collect()),
                        })
                    })
                    .collect::<Result<_, String>>()?,
            },
        };
        Ok(Model { source, general, by_shape: HashMap::new(), declared, info })
    }

    /// Runs the model on `data`, one frame per input of `run`, in any order:
    /// they are matched to the model's inputs by name.
    fn run(&mut self, run: &Run, data: &[Vec<u8>]) -> Result<Vec<(Header, Vec<u8>)>, String> {
        let wanted: Vec<&str> = self.declared.iter().map(|input| input.name.as_str()).collect();
        if run.inputs.len() != wanted.len() {
            return Err(format!("the model takes {} inputs: {}", wanted.len(), wanted.join(", ")));
        }
        // The given inputs in the model's order.
        let mut ordered = Vec::with_capacity(wanted.len());
        for input in &self.declared {
            let index = run
                .inputs
                .iter()
                .position(|given| given.name == input.name)
                .ok_or_else(|| format!("no input {}; the model takes {}", input.name, wanted.join(", ")))?;
            fits(input, &run.inputs[index])?;
            ordered.push(index);
        }
        let headers: Vec<Header> = ordered.iter().map(|&index| run.inputs[index].clone()).collect();
        let plan = self.plan(&headers)?;
        let mut inputs = TVec::new();
        for &index in &ordered {
            inputs.push(to_tract(&run.inputs[index], &data[index]).map_err(failed)?.into());
        }
        let results = plan.run(inputs).map_err(failed)?;
        let mut outputs = Vec::with_capacity(results.len());
        let mut size = 0u64;
        for (outlet, tensor) in plan.model().output_outlets().map_err(failed)?.iter().zip(results.iter()) {
            let name = typed_label(plan.model(), *outlet);
            let (dtype, data) =
                from_tract(tensor).ok_or_else(|| format!("the output {name} is of a type plugins can't get"))?;
            size += data.len() as u64;
            if size > run.output_limit {
                return Err(format!("the outputs are larger than a call takes at once ({} bytes)", run.output_limit));
            }
            let shape = tensor.shape().iter().map(|&dim| dim as u32).collect();
            outputs.push((Header { name, dtype, shape }, data));
        }
        Ok(outputs)
    }

    /// The plan for inputs of these types and shapes, in the model's order.
    fn plan(&mut self, inputs: &[Header]) -> Result<Arc<TypedRunnableModel>, String> {
        if let Some(plan) = &self.general {
            return Ok(plan.clone());
        }
        if let Some(plan) = self.by_shape.get(inputs) {
            return Ok(plan.clone());
        }
        let mut model = self.source.clone();
        for (index, input) in inputs.iter().enumerate() {
            let shape: Vec<usize> = input.shape.iter().map(|&dim| dim as usize).collect();
            model = model.with_input_fact(index, InferenceFact::dt_shape(datum(input.dtype), shape)).map_err(failed)?;
        }
        let plan = model.into_optimized().and_then(|model| model.into_runnable()).map_err(failed)?;
        if self.by_shape.len() >= SHAPE_PLANS {
            self.by_shape.clear();
        }
        self.by_shape.insert(inputs.to_vec(), plan.clone());
        Ok(plan)
    }
}

/// Whether a given input is of the type and shape the model declares, with
/// a clear message if not (tract's own would name its internals).
fn fits(input: &Declared, header: &Header) -> Result<(), String> {
    let name = &input.name;
    if let Some(declared) = input.datum {
        match dtype(declared) {
            Some(dtype) if dtype != header.dtype => {
                return Err(format!("{name}: the model takes {}, not {}", dtype.name(), header.dtype.name()));
            }
            None => return Err(format!("{name}: the model takes a type plugins can't send ({declared:?})")),
            _ => {}
        }
    }
    let Some(shape) = &input.shape else { return Ok(()) };
    if shape.len() != header.shape.len() {
        return Err(format!("{name}: the model takes {} dimensions, not {}", shape.len(), header.shape.len()));
    }
    for (axis, (declared, given)) in shape.iter().zip(&header.shape).enumerate() {
        if declared.is_some_and(|declared| declared != *given) {
            return Err(format!("{name}: dimension {axis} must be {}, not {given}", declared.unwrap_or(0)));
        }
    }
    Ok(())
}

fn concrete(datum: &TypeFactoid) -> Option<DatumType> {
    match datum {
        GenericFactoid::Only(datum) => Some(*datum),
        GenericFactoid::Any => None,
    }
}

/// A dimension, if it is a number.
fn dim(dim: &DimFact) -> Option<u32> {
    match dim {
        GenericFactoid::Only(dim) => dim.to_i64().ok().and_then(|dim| u32::try_from(dim).ok()),
        GenericFactoid::Any => None,
    }
}

/// The name of an input or output: its label, or its node's name.
fn label(model: &InferenceModel, outlet: OutletId) -> String {
    model.outlet_label(outlet).unwrap_or(&model.node(outlet.node).name).to_owned()
}

fn typed_label(model: &TypedModel, outlet: OutletId) -> String {
    model.outlet_label(outlet).unwrap_or(&model.node(outlet.node).name).to_owned()
}

fn typed_info(model: &TypedModel, outlet: OutletId) -> TractResult<TensorInfo> {
    let fact = model.outlet_fact(outlet)?;
    let shape = fact.shape.iter().map(|dim| dim.to_i64().ok().and_then(|dim| u32::try_from(dim).ok())).collect();
    Ok(TensorInfo { name: typed_label(model, outlet), dtype: dtype(fact.datum_type), shape: Some(shape) })
}

fn datum(dtype: DType) -> DatumType {
    match dtype {
        DType::F32 => DatumType::F32,
        DType::F64 => DatumType::F64,
        DType::I32 => DatumType::I32,
        DType::I64 => DatumType::I64,
        DType::I8 => DatumType::I8,
        DType::U8 => DatumType::U8,
        DType::Bool => DatumType::Bool,
    }
}

fn dtype(datum: DatumType) -> Option<DType> {
    Some(match datum {
        DatumType::F32 => DType::F32,
        DatumType::F64 => DType::F64,
        DatumType::I32 => DType::I32,
        DatumType::I64 => DType::I64,
        DatumType::I8 => DType::I8,
        DatumType::U8 => DType::U8,
        DatumType::Bool => DType::Bool,
        _ => return None,
    })
}

/// A tract tensor of checked data (its size fits the shape: the server
/// checked it).
fn to_tract(header: &Header, data: &[u8]) -> TractResult<Tensor> {
    fn values<T, const N: usize>(data: &[u8], from: fn([u8; N]) -> T) -> Vec<T> {
        data.as_chunks::<N>().0.iter().map(|bytes| from(*bytes)).collect()
    }
    let shape: Vec<usize> = header.shape.iter().map(|&dim| dim as usize).collect();
    match header.dtype {
        DType::F32 => Tensor::from_shape(&shape, &values(data, f32::from_le_bytes)),
        DType::F64 => Tensor::from_shape(&shape, &values(data, f64::from_le_bytes)),
        DType::I32 => Tensor::from_shape(&shape, &values(data, i32::from_le_bytes)),
        DType::I64 => Tensor::from_shape(&shape, &values(data, i64::from_le_bytes)),
        DType::I8 => Tensor::from_shape(&shape, &values(data, i8::from_le_bytes)),
        DType::U8 => Tensor::from_shape(&shape, data),
        DType::Bool => Tensor::from_shape(&shape, &data.iter().map(|&byte| byte != 0).collect::<Vec<_>>()),
    }
}

/// The type and little-endian bytes of a tract tensor; none for a type
/// plugins can't get.
fn from_tract(tensor: &Tensor) -> Option<(DType, Vec<u8>)> {
    fn bytes<T: Datum + Copy, const N: usize>(tensor: &Tensor, to: fn(T) -> [u8; N]) -> Option<Vec<u8>> {
        Some(tensor.to_plain_array_view::<T>().ok()?.iter().flat_map(|&value| to(value)).collect())
    }
    let dtype = dtype(tensor.datum_type())?;
    let data = match dtype {
        DType::F32 => bytes(tensor, f32::to_le_bytes),
        DType::F64 => bytes(tensor, f64::to_le_bytes),
        DType::I32 => bytes(tensor, i32::to_le_bytes),
        DType::I64 => bytes(tensor, i64::to_le_bytes),
        DType::I8 => bytes(tensor, i8::to_le_bytes),
        DType::U8 => bytes(tensor, u8::to_le_bytes),
        DType::Bool => Some(tensor.to_plain_array_view::<bool>().ok()?.iter().map(|&value| u8::from(value)).collect()),
    }?;
    Some((dtype, data))
}
