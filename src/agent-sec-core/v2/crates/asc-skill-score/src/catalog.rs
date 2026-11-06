//! Compact repetitive syscall facts for the Judge while retaining the complete evidence catalog.

use crate::{
    Result,
    contracts::{flag, text},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn judge_view(
    catalog: &BTreeMap<String, String>,
    required: &BTreeSet<String>,
) -> Result<(BTreeMap<String, String>, Value)> {
    let mut result = BTreeMap::new();
    let mut groups: BTreeMap<String, (String, u64, Value)> = BTreeMap::new();
    let address =
        regex::Regex::new("0x[0-9a-fA-F]+").map_err(|e| crate::Error::Runtime(e.to_string()))?;
    let destination = regex::Regex::new(r"\{sa_family=.*?\}")
        .map_err(|e| crate::Error::Runtime(e.to_string()))?;
    for (reference, content) in catalog {
        if !reference.starts_with("event:") {
            result.insert(reference.clone(), content.clone());
            continue;
        }
        let event: Value = serde_json::from_str(content)?;
        let mut view = event.clone();
        if flag(&event, "matched") {
            result.insert(reference.clone(), content.clone());
            continue;
        }
        match text(&event, "kind") {
            "file" => {
                if !flag(&event, "write")
                    && (!matches!(text(&event, "syscall"), "open" | "openat" | "openat2")
                        || text(&event, "args").contains("O_DIRECTORY")
                        || ["ENOENT", "ENOTDIR"]
                            .iter()
                            .any(|code| text(&event, "result").contains(code)))
                    && !required.contains(reference)
                {
                    continue;
                }
                if let Some(fields) = view.as_object_mut() {
                    fields.remove("args");
                }
                if text(&event, "outcome") == "succeeded" {
                    view["result"] = "succeeded".into();
                }
            }
            "network" => {
                if let Some(fields) = view.as_object_mut() {
                    fields.remove("target");
                }
                if text(&event, "syscall") != "connect" {
                    let args = text(&view, "args").to_owned();
                    if let Some(fields) = view.as_object_mut() {
                        fields.remove("args");
                    }
                    view["socket"] = args.split(',').next().unwrap_or_default().into();
                    view["destination"] = destination
                        .find(&args)
                        .map_or("see socket peer", |m| m.as_str())
                        .into();
                    view["payload"] =
                        "retained in raw event and scanner input, omitted from Judge view".into();
                }
            }
            _ => {}
        }
        let mut identity = view.clone();
        if let Some(fields) = identity.as_object_mut() {
            for key in ["time", "pid", "ppid", "id"] {
                fields.remove(key);
            }
        }
        let signature = address
            .replace_all(&serde_json::to_string(&identity)?, "<address>")
            .into_owned();
        if let Some((_, count, _)) = groups.get_mut(&signature) {
            *count += 1;
            if required.contains(reference) {
                result.insert(reference.clone(), serde_json::to_string(&view)?);
            }
        } else {
            groups.insert(signature, (reference.clone(), 1, view));
        }
    }
    for (reference, count, mut view) in groups.into_values() {
        view["equivalent_event_count"] = count.into();
        result.insert(reference, serde_json::to_string(&view)?);
    }
    let stats = json!({"raw_references":catalog.len(),"judge_references":result.len(),"raw_characters":catalog.values().map(String::len).sum::<usize>(),"judge_characters":result.values().map(String::len).sum::<usize>()});
    Ok((result, stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn required_duplicate_and_positive_refs_survive_compaction() -> Result<()> {
        let catalog=BTreeMap::from([
            ("event:p:1".into(),json!({"kind":"file","syscall":"openat","outcome":"succeeded","result":"4","target":"/app/work/data","id":"event:p:1","pid":4}).to_string()),
            ("event:p:2".into(),json!({"kind":"file","syscall":"openat","outcome":"succeeded","result":"5","target":"/app/work/data","id":"event:p:2","pid":5}).to_string()),
            ("event:p:3".into(),json!({"kind":"file","syscall":"stat","matched":true}).to_string()),
        ]);
        let (view, _) = judge_view(&catalog, &BTreeSet::new())?;
        assert_eq!(view.len(), 2);
        let (view, _) = judge_view(&catalog, &BTreeSet::from(["event:p:2".into()]))?;
        assert_eq!(view.len(), 3);
        assert_eq!(catalog.len(), 3);
        Ok(())
    }
}
