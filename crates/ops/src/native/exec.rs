//! External tools, called without a shell, so a value never becomes shell syntax.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// Runs a tool and returns its standard output. A non-zero exit is an error that holds its standard error.
pub fn output(program: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("cannot run {program}"))?;
    if !out.status.success() {
        bail!(
            "{program} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Runs a tool that prints to the terminal of the caller.
pub fn status(program: &str, args: &[&str], envs: &[(&str, &str)]) -> Result<()> {
    let result = Command::new(program)
        .args(args)
        .envs(envs.iter().copied())
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("cannot run {program}"))?;
    if !result.success() {
        bail!("{program} failed with {result}");
    }
    Ok(())
}

/// Runs curl with its config on standard input, so a credential never appears in the process list.
pub fn curl(config: &str, args: &[&str]) -> Result<(bool, String)> {
    let mut child = Command::new("curl")
        .arg("--config")
        .arg("-")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot run curl")?;
    child
        .stdin
        .take()
        .context("curl has no standard input")?
        .write_all(config.as_bytes())
        .context("cannot write the curl config")?;
    let out = child.wait_with_output().context("cannot read curl")?;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

/// Quotes a value for a curl config file.
pub fn curl_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curl_quote_escapes_a_quote_and_a_backslash() {
        assert_eq!(curl_quote(r#"a"b\c"#), r#""a\"b\\c""#);
    }
}
