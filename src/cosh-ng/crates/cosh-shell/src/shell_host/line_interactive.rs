use std::io::{self, BufRead, Write};

use crate::shell_host::{run_streaming_line_bash, ShellHostConfig, ShellHostOutput};

#[derive(Debug)]
pub struct LineInteractiveOutput {
    pub shell: ShellHostOutput,
    pub rendered_output: Vec<u8>,
}

pub fn run_line_interactive_bash<R, W>(
    config: &ShellHostConfig,
    input: R,
    mut output: W,
) -> io::Result<LineInteractiveOutput>
where
    R: BufRead,
    W: Write,
{
    let mut rendered_output = Vec::new();
    let shell = run_streaming_line_bash(
        config,
        input,
        TeeWriter::new(&mut output, &mut rendered_output),
    )?;

    Ok(LineInteractiveOutput {
        shell,
        rendered_output,
    })
}

struct TeeWriter<'a, W> {
    output: &'a mut W,
    copy: &'a mut Vec<u8>,
}

impl<'a, W> TeeWriter<'a, W> {
    fn new(output: &'a mut W, copy: &'a mut Vec<u8>) -> Self {
        Self { output, copy }
    }
}

impl<W: Write> Write for TeeWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.output.write_all(buf)?;
        self.copy.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}
