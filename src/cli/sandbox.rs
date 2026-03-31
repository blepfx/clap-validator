use crate::cli::timebase;
use crate::commands::Verbosity;
use crate::fuzz::SandboxedFuzzerChunk;
use crate::plugin::index::SandboxedScanLibrary;
use crate::validator::SandboxedValidation;
use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use wait_timeout::ChildExt;

#[derive(Debug)]
pub struct SandboxConfig {
    pub hide_output: bool,
    pub verbosity: Verbosity,
    pub timeout: Option<Duration>,
}

#[derive(Serialize, Deserialize, Args)]
pub struct SandboxPayload {
    sandbox_id: String,
    sandbox_data: String,
    output_file: String,
}

pub trait SandboxOperation: Serialize + DeserializeOwned {
    const ID: &'static str;

    type Result: Serialize + DeserializeOwned;

    fn run(&self) -> Self::Result;

    fn invoke(&self, config: Option<SandboxConfig>) -> Result<Self::Result> {
        let config = match config {
            Some(config) => config,
            None => return Ok(self.run()),
        };

        let output_file = tempfile::Builder::new()
            .suffix(".json")
            .tempfile()
            .context("Could not create a temporary file path")?
            .into_temp_path();

        let mut command = std::process::Command::new(std::env::current_exe()?);

        command.env(
            "CLAP_VALIDATOR_TIMEBASE",
            timebase()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64()
                .to_string(),
        );
        command.arg("--verbosity");
        command.arg(config.verbosity.to_possible_value().unwrap().get_name());
        command.arg("sandbox");
        command.arg(Self::ID);
        command.arg(serde_json::to_string(&self)?);
        command.arg(output_file.to_str().unwrap());

        if config.hide_output {
            command.stdout(std::process::Stdio::null());
            command.stderr(std::process::Stdio::null());
        }

        let status = match config.timeout {
            None => command.spawn()?.wait()?,
            Some(timeout) => match command.spawn()?.wait_timeout(timeout)? {
                Some(status) => status,
                None => anyhow::bail!("Timed out after {} seconds", timeout.as_secs_f64()),
            },
        };

        if !status.success() {
            anyhow::bail!("{}", status);
        }

        let output = std::fs::read_to_string(&output_file)?;
        let result: Self::Result = serde_json::from_str(&output)?;

        Ok(result)
    }
}

impl SandboxPayload {
    pub fn dispatch(self) -> Result<()> {
        fn dispatch<T: SandboxOperation>(payload: &SandboxPayload) -> Result<()> {
            let operation: T = serde_json::from_str(&payload.sandbox_data)?;
            let result = operation.run();
            std::fs::write(&payload.output_file, serde_json::to_string(&result)?)?;
            Ok(())
        }

        match self.sandbox_id.as_str() {
            SandboxedScanLibrary::ID => dispatch::<SandboxedScanLibrary>(&self)?,
            SandboxedValidation::ID => dispatch::<SandboxedValidation>(&self)?,
            SandboxedFuzzerChunk::ID => dispatch::<SandboxedFuzzerChunk>(&self)?,
            _ => anyhow::bail!("Unknown sandbox ID"),
        };

        Ok(())
    }
}
