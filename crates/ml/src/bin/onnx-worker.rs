//! `rustcloud onnx-worker` for this crate's tests, which have no `rustcloud` binary.

fn main() {
    rustcloud_ml::worker::main(std::env::args_os().skip(1).collect())
}
