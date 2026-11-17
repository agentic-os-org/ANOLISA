const HEADER: &str = "Gradle templates (id<TAB>fields; interleave with literals): ";

// Deliberately independent of the producer: no Gradle recognizers, edit
// plans, regexes or formatting helpers are shared with this decoder.
/// Reconstruct every line without using the producer's recognizers.
pub fn decode(input: &str) -> String {
    let mut lines = input.split_inclusive('\n');
    let mut output = String::new();
    while let Some(line) = lines.next() {
        let Some(header) = line.strip_prefix(HEADER) else {
            output.push_str(line);
            continue;
        };
        let header: serde_json::Value = serde_json::from_str(header).unwrap();
        let literals = header["literals"].as_array().unwrap();
        for _ in 0..header["rows"].as_u64().unwrap() {
            let row = lines.next().unwrap().strip_suffix('\n').unwrap();
            let mut fields = row.split('\t');
            let id: usize = fields.next().unwrap().parse().unwrap();
            let parts = literals[id].as_array().unwrap();
            output.push_str(parts[0].as_str().unwrap());
            let fields = fields.collect::<Vec<_>>();
            assert_eq!(fields.len() + 1, parts.len());
            for (field, literal) in fields.iter().zip(&parts[1..]) {
                output.push_str(field);
                output.push_str(literal.as_str().unwrap());
            }
            output.push('\n');
        }
    }
    output
}
