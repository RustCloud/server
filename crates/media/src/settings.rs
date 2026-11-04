//! `[media]`: the media engine's settings.

use rustcloud_core::{Error, Field, Result, Settings};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MediaSettings {
    /// Whether plugins may use the engine at all.
    pub enabled: bool,
    /// Tools that run at once; 0: half the processor cores. Takes effect
    /// after a restart.
    pub workers: u32,
    /// Memory of one tool process, in MiB.
    pub memory_mib: u32,
    /// Longest one operation may take, in seconds.
    pub time_seconds: u32,
    /// Largest picture or video frame it takes, in megapixels.
    pub max_megapixels: u32,
}

impl Default for MediaSettings {
    fn default() -> Self {
        Self { enabled: true, workers: 0, memory_mib: 1024, time_seconds: 30, max_megapixels: 200 }
    }
}

impl MediaSettings {
    /// How many tools run at once.
    pub fn workers(&self) -> usize {
        match self.workers {
            0 => std::thread::available_parallelism().map_or(1, |cores| (cores.get() / 2).max(1)),
            workers => workers as usize,
        }
    }
}

impl Settings for MediaSettings {
    const SECTION: &'static str = "media";

    fn fields() -> Vec<Field> {
        vec![
            Field::bool("enabled"),
            Field::number("workers", "processes"),
            Field::number("memory_mib", "MiB"),
            Field::number("time_seconds", "s"),
            Field::number("max_megapixels", "MP"),
        ]
    }

    fn validate(&self) -> Result<()> {
        let check =
            |ok: bool, message: &str| if ok { Ok(()) } else { Err(Error::bad_request("INVALID_SETTINGS", message)) };
        check(self.workers <= 64, "workers must be from 0 to 64")?;
        check((64..=16384).contains(&self.memory_mib), "memory_mib must be from 64 to 16384")?;
        check((1..=600).contains(&self.time_seconds), "time_seconds must be from 1 to 600")?;
        check((1..=10_000).contains(&self.max_megapixels), "max_megapixels must be from 1 to 10000")
    }
}
