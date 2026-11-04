//! The engine with real workers (the `onnx-worker` binary of this crate) and
//! tiny models built here.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use prost::Message;
use rustcloud_core::Live;
use rustcloud_ml::{DType, Launcher, ModelFile, NamedTensor, Onnx, OnnxSettings, Priority, Tensor};
use tract_onnx::pb;

fn launcher() -> Launcher {
    Launcher { program: PathBuf::from(env!("CARGO_BIN_EXE_onnx-worker")), prefix: Vec::new() }
}

/// An input or output of `elem_type` (1: f32, 7: i64); `None` dimensions are open.
fn value(name: &str, elem_type: i32, dims: &[Option<i64>]) -> pb::ValueInfoProto {
    use pb::tensor_shape_proto::{dimension::Value, Dimension};
    let dim = dims
        .iter()
        .map(|dim| Dimension {
            denotation: String::new(),
            value: Some(match dim {
                Some(size) => Value::DimValue(*size),
                None => Value::DimParam("n".into()),
            }),
        })
        .collect();
    let tensor = pb::type_proto::Tensor { elem_type, shape: Some(pb::TensorShapeProto { dim }) };
    let r#type = pb::TypeProto { denotation: String::new(), value: Some(pb::type_proto::Value::TensorType(tensor)) };
    pb::ValueInfoProto { name: name.into(), r#type: Some(r#type), doc_string: String::new() }
}

/// A model of one operation.
fn model(op: &str, inputs: Vec<pb::ValueInfoProto>, output: pb::ValueInfoProto) -> Vec<u8> {
    let node = pb::NodeProto {
        input: inputs.iter().map(|input| input.name.clone()).collect(),
        output: vec![output.name.clone()],
        op_type: op.into(),
        ..Default::default()
    };
    let graph =
        pb::GraphProto { name: op.into(), node: vec![node], input: inputs, output: vec![output], ..Default::default() };
    let opset = pb::OperatorSetIdProto { domain: String::new(), version: 13 };
    pb::ModelProto { ir_version: 7, opset_import: vec![opset], graph: Some(graph), ..Default::default() }
        .encode_to_vec()
}

/// `sum = a + b`, f32 of shape `[n, 3]`.
fn add_model() -> Vec<u8> {
    let shape = [None, Some(3)];
    model("Add", vec![value("a", 1, &shape), value("b", 1, &shape)], value("sum", 1, &shape))
}

/// `same = ids`, i64 of shape `[2]`.
fn identity_model() -> Vec<u8> {
    model("Identity", vec![value("ids", 7, &[Some(2)])], value("same", 7, &[Some(2)]))
}

/// A model file on disk and how many times the engine opened it.
struct Stored {
    file: tempfile::NamedTempFile,
    model: ModelFile,
    opened: AtomicUsize,
}

impl Stored {
    fn new(app: &str, bytes: &[u8]) -> Self {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(bytes).unwrap();
        let model =
            ModelFile { app_id: app.into(), path: "models/m.onnx".into(), sha256: format!("{:x}", bytes.len()) };
        Self { file, model, opened: AtomicUsize::new(0) }
    }

    fn open(&self) -> impl std::future::Future<Output = rustcloud_core::Result<std::fs::File>> + '_ {
        self.opened.fetch_add(1, Ordering::SeqCst);
        async move { Ok(std::fs::File::open(self.file.path()).unwrap()) }
    }
}

async fn engine(settings: OnnxSettings) -> Arc<Onnx> {
    let onnx = Onnx::start(launcher(), Arc::new(Live::new(settings))).await;
    assert!(onnx.status().available, "{:?}", onnx.status());
    onnx
}

fn floats(tensor: &Tensor) -> Vec<f32> {
    tensor.data.as_chunks::<4>().0.iter().map(|bytes| f32::from_le_bytes(*bytes)).collect()
}

fn input(name: &str, shape: Vec<u32>, values: &[f32]) -> NamedTensor {
    NamedTensor { name: name.into(), tensor: Tensor::from_f32(shape, values) }
}

#[tokio::test]
async fn runs_a_model_and_keeps_it_loaded() {
    let onnx = engine(OnnxSettings::default()).await;
    assert!(onnx.status().layers.is_some_and(|layers| layers.seccomp));
    let stored = Stored::new("app", &add_model());

    let info = onnx.info(&stored.model, || stored.open(), Priority::Request).await.unwrap();
    let names = |list: &[rustcloud_ml::TensorInfo]| list.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
    assert_eq!((names(&info.inputs), names(&info.outputs)), (vec!["a".into(), "b".into()], vec!["sum".into()]));
    assert_eq!(info.inputs[0].shape, Some(vec![None, Some(3)]), "the batch is left open");
    assert_eq!(info.inputs[0].dtype, Some(DType::F32));

    // Inputs by name, in any order; any batch.
    for batch in [1u32, 2] {
        let values: Vec<f32> = (0..batch * 3).map(|v| v as f32).collect();
        let inputs = vec![input("b", vec![batch, 3], &values), input("a", vec![batch, 3], &values)];
        let outputs = onnx.run(&stored.model, || stored.open(), inputs, 1 << 20, Priority::Request).await.unwrap();
        assert_eq!(outputs[0].name, "sum");
        assert_eq!(outputs[0].tensor.shape, [batch, 3]);
        assert_eq!(floats(&outputs[0].tensor), values.iter().map(|v| v * 2.0).collect::<Vec<_>>());
    }
    assert_eq!(stored.opened.load(Ordering::SeqCst), 1, "loaded once for all the calls");
    let loaded = onnx.loaded();
    assert_eq!((loaded.len(), loaded[0].app_id.as_str(), loaded[0].busy), (1, "app", false));

    onnx.forget("app");
    assert!(onnx.loaded().is_empty());
}

#[tokio::test]
async fn wrong_inputs_are_explained_and_the_model_stays() {
    let onnx = engine(OnnxSettings::default()).await;
    let stored = Stored::new("app", &add_model());
    let run = |inputs: Vec<NamedTensor>| onnx.run(&stored.model, || stored.open(), inputs, 1 << 20, Priority::Request);
    let ones = [1.0; 6];

    let missing = run(vec![input("a", vec![2, 3], &ones), input("c", vec![2, 3], &ones)]).await.unwrap_err();
    assert_eq!(missing.code, "ONNX_FAILED");
    assert!(missing.message.contains("no input b"), "{}", missing.message);

    let shape = run(vec![input("a", vec![3, 2], &ones), input("b", vec![3, 2], &ones)]).await.unwrap_err();
    assert!(shape.message.contains("dimension 1 must be 3, not 2"), "{}", shape.message);

    let ids =
        NamedTensor { name: "b".into(), tensor: Tensor { dtype: DType::I64, shape: vec![2, 3], data: vec![0; 48] } };
    let dtype = run(vec![input("a", vec![2, 3], &ones), ids]).await.unwrap_err();
    assert!(dtype.message.contains("the model takes f32, not i64"), "{}", dtype.message);

    let short =
        NamedTensor { name: "b".into(), tensor: Tensor { dtype: DType::F32, shape: vec![2, 3], data: vec![0; 5] } };
    let bytes = run(vec![input("a", vec![2, 3], &ones), short]).await.unwrap_err();
    assert_eq!(bytes.code, "ONNX_INVALID_INPUT");

    let too_large = onnx
        .run(
            &stored.model,
            || stored.open(),
            vec![input("a", vec![2, 3], &ones), input("b", vec![2, 3], &ones)],
            8,
            Priority::Request,
        )
        .await
        .unwrap_err();
    assert!(too_large.message.contains("larger than a call takes"), "{}", too_large.message);

    let fine = run(vec![input("a", vec![2, 3], &ones), input("b", vec![2, 3], &ones)]).await;
    assert!(fine.is_ok(), "{fine:?}");
    assert_eq!(stored.opened.load(Ordering::SeqCst), 1, "errors don't unload the model");
}

#[tokio::test]
async fn integers_pass_through() {
    let onnx = engine(OnnxSettings::default()).await;
    let stored = Stored::new("app", &identity_model());
    let data: Vec<u8> = [7i64, -1].iter().flat_map(|v| v.to_le_bytes()).collect();
    let ids =
        NamedTensor { name: "ids".into(), tensor: Tensor { dtype: DType::I64, shape: vec![2], data: data.clone() } };
    let outputs = onnx.run(&stored.model, || stored.open(), vec![ids], 1 << 20, Priority::Request).await.unwrap();
    assert_eq!((outputs[0].tensor.dtype, &outputs[0].tensor.data), (DType::I64, &data));
}

#[tokio::test]
async fn a_broken_model_is_refused() {
    let onnx = engine(OnnxSettings::default()).await;
    let stored = Stored::new("app", b"this is no model");
    let err = onnx.info(&stored.model, || stored.open(), Priority::Request).await.unwrap_err();
    assert_eq!(err.code, "ONNX_FAILED");
    assert!(err.message.starts_with("the model can't be loaded"), "{}", err.message);
    assert!(onnx.loaded().is_empty());
}

#[tokio::test]
async fn one_more_model_unloads_the_one_unused_longest() {
    let onnx = engine(OnnxSettings { workers: 1, ..OnnxSettings::default() }).await;
    let first = Stored::new("first", &add_model());
    let second = Stored::new("second", &identity_model());
    onnx.info(&first.model, || first.open(), Priority::Request).await.unwrap();
    onnx.info(&second.model, || second.open(), Priority::Request).await.unwrap();
    let loaded: Vec<String> = onnx.loaded().into_iter().map(|model| model.app_id).collect();
    assert_eq!(loaded, ["second"]);
}

#[tokio::test]
async fn without_keeping_a_model_is_unloaded_after_its_run() {
    let onnx = engine(OnnxSettings { keep_loaded_seconds: 0, ..OnnxSettings::default() }).await;
    let stored = Stored::new("app", &identity_model());
    let ids =
        NamedTensor { name: "ids".into(), tensor: Tensor { dtype: DType::I64, shape: vec![2], data: vec![0; 16] } };
    onnx.run(&stored.model, || stored.open(), vec![ids], 1 << 20, Priority::Request).await.unwrap();
    assert!(onnx.loaded().is_empty());
}

#[tokio::test]
async fn too_large_a_model_is_refused_before_loading() {
    let onnx = engine(OnnxSettings { max_model_mib: 1, ..OnnxSettings::default() }).await;
    let stored = Stored::new("app", &vec![0; 2 << 20]);
    let err = onnx.info(&stored.model, || stored.open(), Priority::Request).await.unwrap_err();
    assert!(err.message.contains("up to 1 MiB"), "{}", err.message);
}

#[tokio::test]
async fn turned_off_it_says_so() {
    let settings = OnnxSettings { enabled: false, ..OnnxSettings::default() };
    let onnx = Onnx::start(launcher(), Arc::new(Live::new(settings))).await;
    assert!(!onnx.status().available);
    let stored = Stored::new("app", &add_model());
    let err = onnx.info(&stored.model, || stored.open(), Priority::Request).await.unwrap_err();
    assert_eq!(err.code, "ONNX_UNSUPPORTED");
}
