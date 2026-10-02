use super::non_empty_lines;

pub(super) fn is_stack_trace(scan: &str) -> bool {
    let mut lines = non_empty_lines(scan);
    let Some(first) = lines.next() else {
        return false;
    };
    if first.starts_with("Traceback (most recent call last") {
        return true;
    }
    if first.starts_with("thread '") && first.contains("panicked at") {
        return true;
    }
    if first.starts_with("panic:") || first.starts_with("goroutine ") {
        return true;
    }
    // Java/JS style: an exception line followed by indented `at` frames.
    let Some(second) = lines.next() else {
        return false;
    };
    (first.contains("Exception") || first.contains("Error"))
        && second.trim_start().starts_with("at ")
}
