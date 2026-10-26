//! Fixed executable + explicit argv; bounded pipes and a process-group deadline.
use ndimon_privd_proto::{ActionOutput, Error};
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::Arc,
};
const MAX_OUTPUT: usize = 512 * 1024;
pub trait Runner: std::fmt::Debug + Send + Sync {
    fn run(
        &self,
        program: &str,
        args: &[String],
        input: Option<&[u8]>,
    ) -> Result<ActionOutput, Error>;
    /// Tests share the privd effect boundary. Production retains the network
    /// engine's per-command deadlines, output bounds and nonzero probe results.
    fn network(
        &self,
        command: &ndimon_netengine::host::CommandSpec,
    ) -> ndimon_netengine::Result<ndimon_netengine::host::CommandOutput> {
        use ndimon_netengine::host::CommandOutput;
        match self.run(
            command.tool.program(),
            &command.args,
            command.input.as_deref(),
        ) {
            Ok(output) => Ok(CommandOutput {
                code: 0,
                stdout: output.stdout,
                stderr: output.stderr,
            }),
            Err(Error::Failed {
                reason,
                exit_code: Some(code),
                ..
            }) => Ok(CommandOutput {
                code,
                stdout: String::new(),
                stderr: reason,
            }),
            Err(error) => Err(ndimon_types::ApiError::ApplyFailed {
                domain: ndimon_types::Domain::Network,
                step: "command".into(),
                reason: error.to_string(),
            }),
        }
    }
    fn hook(
        &self,
        program: &str,
        args: &[String],
        directory: &std::path::Path,
        state: &std::path::Path,
    ) -> Result<ActionOutput, Error> {
        let _ = (directory, state);
        self.run(program, args, None)
    }
}
#[derive(Debug)]
pub struct Processes;
impl Runner for Processes {
    fn network(
        &self,
        command: &ndimon_netengine::host::CommandSpec,
    ) -> ndimon_netengine::Result<ndimon_netengine::host::CommandOutput> {
        #[cfg(feature = "harness")]
        return ndimon_netengine::host::CommandRunner::run(
            &ndimon_netengine::harness::ContainerProcesses,
            command,
        );
        #[cfg(not(feature = "harness"))]
        ndimon_netengine::host::CommandRunner::run(&ndimon_netengine::host::Processes, command)
    }
    fn run(
        &self,
        program: &str,
        args: &[String],
        input: Option<&[u8]>,
    ) -> Result<ActionOutput, Error> {
        execute(program, args, input, None)
    }
    fn hook(
        &self,
        program: &str,
        args: &[String],
        directory: &std::path::Path,
        state: &std::path::Path,
    ) -> Result<ActionOutput, Error> {
        execute(program, args, None, Some((directory, state)))
    }
}
fn execute(
    program: &str,
    args: &[String],
    input: Option<&[u8]>,
    plugin: Option<(&std::path::Path, &std::path::Path)>,
) -> Result<ActionOutput, Error> {
    assert!(program.starts_with('/'));
    let mut command = Command::new("/usr/bin/timeout");
    command
        .args(["--kill-after=5s", "620s", program])
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LC_ALL", "C")
        .env("HOME", "/root")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some((directory, state)) = plugin {
        command
            .current_dir(directory)
            .env("NDIMON_PLUGIN_DIR", directory)
            .env("NDIMON_PLUGIN_STATE", state);
    }
    let mut child = command.spawn().map_err(|e| Error::failed("spawn", e))?;
    fn capture(mut pipe: impl Read) -> Result<Vec<u8>, std::io::Error> {
        let mut bytes = vec![];
        pipe.by_ref()
            .take(MAX_OUTPUT as u64 + 1)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::failed("pipe", "stdout missing"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::failed("pipe", "stderr missing"))?;
    let out = std::thread::spawn(move || capture(stdout));
    let err = std::thread::spawn(move || capture(stderr));
    let input_result = if let Some(input) = input {
        child
            .stdin
            .take()
            .ok_or_else(|| Error::failed("pipe", "stdin missing"))?
            .write_all(input)
    } else {
        Ok(())
    };
    let status = child.wait().map_err(|e| Error::failed("wait", e))?;
    let out = out
        .join()
        .map_err(|_| Error::failed("stdout", "reader panicked"))?
        .map_err(|e| Error::failed("stdout", e))?;
    let err = err
        .join()
        .map_err(|_| Error::failed("stderr", "reader panicked"))?
        .map_err(|e| Error::failed("stderr", e))?;
    if out.len() > MAX_OUTPUT || err.len() > MAX_OUTPUT {
        return Err(Error::failed(
            "output",
            "program output exceeds 512 KiB per stream",
        ));
    }
    if !status.success() {
        return Err(Error::Failed {
            step: program.into(),
            reason: String::from_utf8_lossy(&err).chars().take(4096).collect(),
            exit_code: status.code(),
        });
    }
    input_result.map_err(|e| Error::failed("stdin", e))?;
    Ok(ActionOutput {
        stdout: String::from_utf8_lossy(&out).into(),
        stderr: String::from_utf8_lossy(&err).into(),
        ..Default::default()
    })
}
pub fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).into()).collect()
}
pub type SharedRunner = Arc<dyn Runner>;

#[cfg(test)]
mod tests {
    use super::*;
    use ndimon_netengine::host::{CommandSpec, Tool};

    #[derive(Debug)]
    struct Probe(Option<Error>);
    impl Runner for Probe {
        fn run(
            &self,
            program: &str,
            args: &[String],
            input: Option<&[u8]>,
        ) -> Result<ActionOutput, Error> {
            assert_eq!(program, "/usr/sbin/nft");
            assert_eq!(args, ["-f", "-"]);
            assert_eq!(input, Some(b"fixture rules".as_slice()));
            match &self.0 {
                Some(error) => Err(error.clone()),
                None => Ok(ActionOutput {
                    stdout: "output".into(),
                    stderr: "diagnostic".into(),
                    ..Default::default()
                }),
            }
        }
    }
    #[test]
    fn injected_network_commands_preserve_input_output_and_probe_failures() {
        let command = CommandSpec {
            tool: Tool::Nft,
            args: args(&["-f", "-"]),
            input: Some(b"fixture rules".to_vec()),
            timeout_ms: 1000,
            output_limit: 100,
        };
        let output = Probe(None).network(&command).unwrap();
        assert_eq!(
            (output.code, output.stdout.as_str(), output.stderr.as_str()),
            (0, "output", "diagnostic")
        );
        let output = Probe(Some(Error::Failed {
            step: "probe".into(),
            reason: "inactive".into(),
            exit_code: Some(3),
        }))
        .network(&command)
        .unwrap();
        assert_eq!((output.code, output.stderr.as_str()), (3, "inactive"));
        assert!(
            Probe(Some(Error::failed("spawn", "missing")))
                .network(&command)
                .is_err()
        );
    }
}
