use super::*;

fn write_output_file(dir_name: &str, body: &[u8]) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{dir_name}-{}-{unique}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("output");
    fs::write(&path, body).unwrap();
    path
}

fn preview_for(body: &[u8]) -> (String, PathBuf) {
    let path = write_output_file("cosh_hook_preview", body);
    let mut block = make_block("cat output");
    block.output.terminal_output_ref = Some(path.to_string_lossy().to_string());
    let input = runtime::hook_input_from_block(&block);
    (input.output_preview, path)
}

#[test]
fn hook_input_preview_is_byte_bounded_for_huge_single_line() {
    // The spill file exists because the output was large; one line without
    // newlines must not pull the whole file into the hook payload.
    let (preview, path) = preview_for(&vec![b'x'; 256 * 1024]);

    assert_eq!(
        preview.len(),
        64 * 1024,
        "preview must stop at the byte cap"
    );
    assert!(preview.bytes().all(|byte| byte == b'x'));
    let _ = fs::remove_file(&path);
}

#[test]
fn hook_input_preview_keeps_first_fifty_lines_for_normal_output() {
    let body: String = (0..60).map(|i| format!("line-{i}\n")).collect();
    let (preview, path) = preview_for(body.as_bytes());

    let expected: String = (0..50)
        .map(|i| format!("line-{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(preview, expected);
    let _ = fs::remove_file(&path);
}

#[test]
fn hook_input_preview_is_lossy_and_redacted_for_non_utf8_output() {
    let secret = "non-utf8-preview-secret";
    let mut body = format!("token={secret}\n").into_bytes();
    body.extend_from_slice(&[0xff, 0xfe, b'\n']);
    let (preview, path) = preview_for(&body);

    assert!(!preview.is_empty(), "non-UTF-8 output must keep a preview");
    assert!(
        preview.contains('\u{FFFD}'),
        "invalid bytes must decode lossily: {preview:?}"
    );
    assert!(
        !preview.contains(secret),
        "preview must be redacted before reaching HookInput: {preview:?}"
    );
    let _ = fs::remove_file(&path);
}
