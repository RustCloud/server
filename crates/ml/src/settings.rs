//! `[onnx]`: the settings of `use.onnx`.

use rustcloud_core::{Error, Field, Result, Settings};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OnnxSettings {
    /// Whether plugins may use neural networks at all.
    pub enabled: bool,
    /// Models kept loaded at once, each in a process of its own.
    pub workers: u32,
    /// Memory of one model's process, in MiB: the model and its work.
    pub memory_mib: u32,
    /// Longest one run (or loading a model) may take, in seconds.
    pub time_seconds: u32,
    /// How long a model stays loaded after its last run, in seconds: runs
    /// over a list of files load it once. 0: unloaded after each run.
    pub keep_loaded_seconds: u32,
    /// Largest model file, in MiB.
    pub max_model_mib: u32,
}

impl Default for OnnxSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            workers: 4,
            memory_mib: 2048,
            time_seconds: 60,
            keep_loaded_seconds: 300,
            max_model_mib: 1024,
        }
    }
}

impl Settings for OnnxSettings {
    const SECTION: &'static str = "onnx";

    fn fields() -> Vec<Field> {
        vec![
            Field::bool("enabled"),
            Field::number("workers", "models"),
            Field::number("memory_mib", "MiB"),
            Field::number("time_seconds", "s"),
            Field::number("keep_loaded_seconds", "s"),
            Field::number("max_model_mib", "MiB"),
        ]
    }

    fn validate(&self) -> Result<()> {
        let check =
            |ok: bool, message: &str| if ok { Ok(()) } else { Err(Error::bad_request("INVALID_SETTINGS", message)) };
        check((1..=64).contains(&self.workers), "workers must be from 1 to 64")?;
        check((128..=65536).contains(&self.memory_mib), "memory_mib must be from 128 to 65536")?;
        check((1..=600).contains(&self.time_seconds), "time_seconds must be from 1 to 600")?;
        check(self.keep_loaded_seconds <= 86400, "keep_loaded_seconds must be from 0 to 86400")?;
        check((1..=16384).contains(&self.max_model_mib), "max_model_mib must be from 1 to 16384")?;
        check(
            u64::from(self.max_model_mib) < u64::from(self.memory_mib),
            "max_model_mib must be less than memory_mib: the model is loaded into that memory",
        )
    }
}
