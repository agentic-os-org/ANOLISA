use crate::types::AgentEvent;

pub(super) fn fake_markdown_response(input: &str, run_id: &str) -> Option<Vec<AgentEvent>> {
    if input.contains("markdown indented code") {
        return Some(vec![
            AgentEvent::StatusChanged {
                run_id: run_id.to_string(),
                phase: "rendering".to_string(),
                message: "returning indented markdown code guidance".to_string(),
            },
            AgentEvent::TextDelta {
                run_id: run_id.to_string(),
                text: "Indented code check\n\n    cargo test --package cosh-shell\n    git status --short\n\nDone.".to_string(),
            },
            AgentEvent::AgentCompleted {
                run_id: run_id.to_string(),
                summary: "markdown indented code fake analysis completed".to_string(),
            },
        ]);
    }

    if input.contains("markdown paragraph") {
        return Some(vec![
            AgentEvent::StatusChanged {
                run_id: run_id.to_string(),
                phase: "rendering".to_string(),
                message: "returning soft-wrapped markdown paragraph".to_string(),
            },
            AgentEvent::TextDelta {
                run_id: run_id.to_string(),
                text: "Paragraph rendering\n\nThis Agent answer is split\nacross multiple source lines with 中文内容\nbut should read as one Markdown paragraph.".to_string(),
            },
            AgentEvent::AgentCompleted {
                run_id: run_id.to_string(),
                summary: "markdown paragraph fake analysis completed".to_string(),
            },
        ]);
    }

    if input.contains("markdown pipe output") {
        return Some(vec![
            AgentEvent::StatusChanged {
                run_id: run_id.to_string(),
                phase: "rendering".to_string(),
                message: "returning markdown pipe output".to_string(),
            },
            AgentEvent::TextDelta {
                run_id: run_id.to_string(),
                text: "Shell output:\n\n| 1 | Virtualization.VirtualMachine | ~1470 MB |\n| 2 | Node | ~572 MB |\n\nDone.".to_string(),
            },
            AgentEvent::AgentCompleted {
                run_id: run_id.to_string(),
                summary: "markdown pipe output fake analysis completed".to_string(),
            },
        ]);
    }

    if input.contains("markdown table") {
        return Some(vec![
            AgentEvent::StatusChanged {
                run_id: run_id.to_string(),
                phase: "rendering".to_string(),
                message: "returning markdown table guidance".to_string(),
            },
            AgentEvent::TextDelta {
                run_id: run_id.to_string(),
                text: "内存占用 Top 10 分析:\n\n| 排名 | 进程 | RSS (MB) | 说明 |\n| --- | --- | --- | --- |\n| 1 | Virtualization.VirtualMachine | ~1470 MB | 虚拟机进程，最大内存消耗者 |\n| 2 | ps aux \\| grep cosh | ~42 MB | escaped pipe 应保留在单元格中 |\n\n关键发现：Qoder 占用最多。".to_string(),
            },
            AgentEvent::AgentCompleted {
                run_id: run_id.to_string(),
                summary: "markdown table fake analysis completed".to_string(),
            },
        ]);
    }

    if input.contains("markdown") {
        return Some(vec![
            AgentEvent::StatusChanged {
                run_id: run_id.to_string(),
                phase: "rendering".to_string(),
                message: "returning markdown guidance".to_string(),
            },
            AgentEvent::TextDelta {
                run_id: run_id.to_string(),
                text: "# Project check\n\n- Run `git status`\n- Build workspace\n  - Use package scoped tests\n  1. Keep shell-first validation repeatable\n1. Review rendered transcript\n\n```bash\ncargo build --workspace\nif test -d crates; then\n  cargo test --package cosh-shell\nfi\n```\n\n> Commands are suggestions only.".to_string(),
            },
            AgentEvent::AgentCompleted {
                run_id: run_id.to_string(),
                summary: "markdown fake analysis completed".to_string(),
            },
        ]);
    }

    None
}
