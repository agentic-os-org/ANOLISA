use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub(crate) fn write_tool_output_ref(dir: &Path, id: &str, text: &str) -> std::io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    let path = dir.join(format!("{id}.txt"));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok(path)
}

pub(super) fn tool_output_detail(
    tool_id: &str,
    stream: &str,
    lines: usize,
    output_ref: Option<&str>,
    text: &str,
    provider_native_shell_command: Option<&str>,
    provider_shell_tool: bool,
) -> String {
    let mut detail = format!("tool: {tool_id}\nstream: {stream}\nlines: {lines}");
    if provider_shell_tool {
        detail.push_str("\nprovider_tool_class: shell");
    }
    if let Some(command) = provider_native_shell_command {
        detail.push_str(&format!("\nprovider_native_shell_command: {command}"));
    }
    let capture_status = if output_ref.is_some() {
        "captured"
    } else {
        "unavailable"
    };
    if let Some(output_ref) = output_ref {
        detail.push_str(&format!("\ndebug_output_ref: {output_ref}"));
    }
    detail.push_str(&format!(
        "\ncapture_status: {capture_status}\noutput_ref: <hidden>"
    ));
    detail.push('\n');
    detail.push_str(text);
    detail
}
