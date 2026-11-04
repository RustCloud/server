//! The sound of a file for a neural network: mono, at a chosen rate, as
//! 32-bit floats.

use std::ffi::OsString;

use rustcloud_core::{Error, Result};
use serde::Serialize;

/// What part of a file's sound to give, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SampleOptions {
    /// Samples per second: 8000–48000 (speech models take 16000).
    pub rate: u32,
    /// Where to start, from the beginning of the file.
    pub start_ms: u64,
    /// How long; none: to the end.
    pub duration_ms: Option<u64>,
}

impl SampleOptions {
    pub(crate) fn check(&self) -> Result<()> {
        if !(8000..=48000).contains(&self.rate) {
            return Err(Error::bad_request("INVALID_OPTIONS", "the rate must be from 8000 to 48000"));
        }
        if self.duration_ms == Some(0) {
            return Err(Error::bad_request("INVALID_OPTIONS", "the duration must not be 0"));
        }
        Ok(())
    }
}

/// The ffmpeg command for the samples: only numbers from checked options.
pub(crate) fn samples_args(options: &SampleOptions) -> Vec<OsString> {
    let seconds = |ms: u64| format!("{}.{:03}", ms / 1000, ms % 1000);
    let mut args: Vec<String> =
        ["-v", "error", "-nostdin", "-protocol_whitelist", "fd", "-threads", "2"].map(str::to_owned).into();
    if options.start_ms > 0 {
        args.extend(["-ss".into(), seconds(options.start_ms)]);
    }
    if let Some(duration) = options.duration_ms {
        args.extend(["-t".into(), seconds(duration)]);
    }
    args.extend(["-i".into(), "fd:".into(), "-vn".into(), "-sn".into(), "-dn".into()]);
    args.extend(["-map".into(), "0:a:0".into(), "-ac".into(), "1".into(), "-ar".into(), options.rate.to_string()]);
    args.extend(["-c:a", "pcm_f32le", "-f", "f32le", "pipe:1"].map(str::to_owned));
    args.into_iter().map(OsString::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_for_mono_floats_of_a_part() {
        let options = SampleOptions { rate: 16000, start_ms: 1500, duration_ms: Some(30_000) };
        let args: Vec<String> = samples_args(&options).into_iter().map(|a| a.into_string().unwrap()).collect();
        let line = args.join(" ");
        assert!(line.contains("-ss 1.500 -t 30.000 -i fd:"), "{line}");
        assert!(line.ends_with("-ac 1 -ar 16000 -c:a pcm_f32le -f f32le pipe:1"), "{line}");
    }

    #[test]
    fn rates_and_durations_are_checked() {
        let ok = SampleOptions { rate: 16000, start_ms: 0, duration_ms: None };
        assert!(ok.check().is_ok());
        assert!(SampleOptions { rate: 100, ..ok }.check().is_err());
        assert!(SampleOptions { duration_ms: Some(0), ..ok }.check().is_err());
    }
}
