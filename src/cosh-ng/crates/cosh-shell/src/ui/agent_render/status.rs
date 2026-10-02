use std::io::{self, Write};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct AgentStatusAnimation {
    enabled: bool,
    visible: bool,
    frame: usize,
    last_render_at: Option<Instant>,
    last_label: Option<String>,
}

impl AgentStatusAnimation {
    pub(super) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            visible: false,
            frame: 0,
            last_render_at: None,
            last_label: None,
        }
    }

    pub fn render<W: Write>(&mut self, output: &mut W, label: &str) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let now = Instant::now();
        let label_changed = self.last_label.as_deref() != Some(label);
        if !label_changed
            && self
                .last_render_at
                .is_some_and(|last| now.duration_since(last) < Duration::from_millis(220))
        {
            return Ok(());
        }

        const FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
        write!(output, "\r\x1b[2K")?;
        write!(output, "{} {}", FRAMES[self.frame % FRAMES.len()], label)?;
        output.flush()?;

        self.frame += 1;
        self.last_render_at = Some(now);
        self.last_label = Some(label.to_string());
        self.visible = true;
        Ok(())
    }

    pub fn clear<W: Write>(&mut self, output: &mut W) -> io::Result<()> {
        if self.enabled && self.visible {
            write!(output, "\r\x1b[2K\r")?;
            output.flush()?;
            self.visible = false;
            self.last_label = None;
        }
        Ok(())
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_same_label_is_throttled_without_clearing() {
        let mut animation = AgentStatusAnimation::new(true);
        let mut output = Vec::new();

        animation
            .render(&mut output, "Thinking")
            .expect("first render");
        let first = output.len();
        animation
            .render(&mut output, "Thinking")
            .expect("second render");

        assert_eq!(output.len(), first);
    }

    #[test]
    fn changed_label_renders_immediately() {
        let mut animation = AgentStatusAnimation::new(true);
        let mut output = Vec::new();

        animation
            .render(&mut output, "Thinking")
            .expect("first render");
        let first = output.len();
        animation
            .render(&mut output, "Thinking: reading file")
            .expect("changed render");

        assert!(output.len() > first);
    }
}
