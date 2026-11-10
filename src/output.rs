//! Formatting of invocation responses.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;

use crate::client::{InvokeResponse, Item};

/// Items up to this many bytes are shown inline in the summary (if they are UTF-8).
pub const INLINE_LIMIT: usize = 256;

pub fn format_size(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else {
        bytesize::ByteSize::b(n).display().iec().to_string()
    }
}

pub fn format_duration(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 1000.0 { format!("{ms:.1} ms") } else { format!("{:.2} s", ms / 1000.0) }
}

fn inline_text(data: &[u8]) -> Option<&str> {
    if data.len() > INLINE_LIMIT {
        return None;
    }
    let s = std::str::from_utf8(data).ok()?;
    let printable = s.chars().all(|c| !c.is_control() || c == '\n' || c == '\t' || c == '\r');
    printable.then_some(s)
}

/// Human-readable summary of every set and item.
pub fn summary(resp: &InvokeResponse) -> String {
    let mut out = String::new();
    if resp.sets.is_empty() {
        out.push_str("(no output sets)\n");
    }
    for (si, set) in resp.sets.iter().enumerate() {
        let n = set.items.len();
        let _ = writeln!(out, "set {si}: {n} item{}", if n == 1 { "" } else { "s" });
        for item in &set.items {
            let _ = writeln!(
                out,
                "  {:?}  key={}  size={}",
                item.identifier,
                item.key,
                format_size(item.data.len() as u64)
            );
            if item.data.is_empty() {
                continue;
            }
            if let Some(text) = inline_text(&item.data) {
                for line in text.trim_end_matches('\n').lines() {
                    let _ = writeln!(out, "    | {line}");
                }
            }
        }
    }
    if let Some(ts) = &resp.timestamps {
        out.push_str("timestamps:\n");
        let pretty = serde_json::from_str::<serde_json::Value>(ts)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .unwrap_or_else(|| ts.clone());
        for line in pretty.lines() {
            let _ = writeln!(out, "  {line}");
        }
    }
    out
}

/// `{ sets: [...], timestamps: <parsed JSON | null> }` with base64 item data.
pub fn to_json(resp: &InvokeResponse) -> serde_json::Value {
    let b64 = base64::engine::general_purpose::STANDARD;
    let sets: Vec<serde_json::Value> = resp
        .sets
        .iter()
        .map(|s| {
            let items: Vec<serde_json::Value> = s
                .items
                .iter()
                .map(|i| {
                    serde_json::json!({
                        "identifier": i.identifier,
                        "key": i.key,
                        "data": b64.encode(&i.data),
                    })
                })
                .collect();
            serde_json::json!({ "items": items })
        })
        .collect();
    let timestamps = match &resp.timestamps {
        None => serde_json::Value::Null,
        // Fall back to the raw string if the server sends something that is not JSON.
        Some(ts) => serde_json::from_str(ts).unwrap_or_else(|_| serde_json::Value::String(ts.clone())),
    };
    serde_json::json!({ "sets": sets, "timestamps": timestamps })
}

/// Turns an item identifier into a safe file name. Identifiers come from the server, so they must
/// not escape the output directory.
fn file_name_for(item: &Item, index: usize) -> String {
    let cleaned: String = item
        .identifier
        .chars()
        .map(|c| if c == '/' || c == '\\' || c == '\0' { '_' } else { c })
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        format!("item-{index}")
    } else {
        cleaned
    }
}

/// Writes each item to `DIR/<set index>/<item>`, and timestamps to `DIR/timestamps.json`.
/// Response sets carry no names, so the set directory is the set's index.
/// Returns the paths written.
pub fn write_out_dir(resp: &InvokeResponse, dir: &Path) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    for (si, set) in resp.sets.iter().enumerate() {
        let set_dir = dir.join(si.to_string());
        std::fs::create_dir_all(&set_dir)
            .with_context(|| format!("cannot create {}", set_dir.display()))?;
        let mut used = HashSet::new();
        for (ii, item) in set.items.iter().enumerate() {
            let mut name = file_name_for(item, ii);
            if !used.insert(name.clone()) {
                name = format!("{name}#{ii}");
                used.insert(name.clone());
            }
            let path = set_dir.join(&name);
            std::fs::write(&path, &item.data)
                .with_context(|| format!("cannot write {}", path.display()))?;
            written.push(path);
        }
    }
    if let Some(ts) = &resp.timestamps {
        let path = dir.join("timestamps.json");
        std::fs::write(&path, ts).with_context(|| format!("cannot write {}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

/// Finds the item for `--raw <SET>:<ITEM>`. SET is the set index. ITEM is matched against the
/// identifiers first and is otherwise taken as the item index.
pub fn select_raw<'a>(resp: &'a InvokeResponse, spec: &str) -> Result<&'a Item> {
    let (set, item) = spec
        .split_once(':')
        .ok_or_else(|| anyhow!("invalid --raw `{spec}`; expected `<SET>:<ITEM>`"))?;
    let si: usize = set
        .parse()
        .map_err(|_| anyhow!("invalid --raw `{spec}`: the set must be an index (0, 1, ...)"))?;
    let set_ref = resp
        .sets
        .get(si)
        .ok_or_else(|| anyhow!("response has {} set(s); there is no set {si}", resp.sets.len()))?;
    if let Some(found) = set_ref.items.iter().find(|i| i.identifier == item) {
        return Ok(found);
    }
    if let Some(found) = item.parse::<usize>().ok().and_then(|ii| set_ref.items.get(ii)) {
        return Ok(found);
    }
    let ids: Vec<String> = set_ref.items.iter().map(|i| format!("{:?}", i.identifier)).collect();
    bail!("set {si} has no item `{item}`; items: [{}]", ids.join(", "))
}

pub fn write_stdout(bytes: &[u8]) -> Result<()> {
    let mut out = std::io::stdout().lock();
    out.write_all(bytes)?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Set;

    fn resp() -> InvokeResponse {
        InvokeResponse {
            sets: vec![
                Set {
                    items: vec![
                        Item { identifier: "msg".into(), key: 0, data: b"hello\n".to_vec() },
                        Item { identifier: "bin".into(), key: 1, data: vec![0, 159, 146, 150] },
                    ],
                },
                Set { items: vec![Item { identifier: "../x".into(), key: 0, data: b"a".to_vec() }] },
            ],
            timestamps: Some("{\"t\":[1,2]}".into()),
        }
    }

    #[test]
    fn summary_shows_text_but_not_binary() {
        let s = summary(&resp());
        assert!(s.contains("set 0: 2 items"));
        assert!(s.contains("\"msg\"  key=0  size=6 B"));
        assert!(s.contains("    | hello"));
        assert!(s.contains("\"bin\"  key=1  size=4 B"));
        assert!(s.contains("timestamps:"));
        assert!(s.contains("\"t\": ["));
    }

    #[test]
    fn json_output() {
        let j = to_json(&resp());
        assert_eq!(j["sets"][0]["items"][0]["data"], "aGVsbG8K");
        assert_eq!(j["timestamps"]["t"][1], 2);
        let no_ts = InvokeResponse { timestamps: None, ..resp() };
        assert!(to_json(&no_ts)["timestamps"].is_null());
    }

    #[test]
    fn raw_selection() {
        let r = resp();
        assert_eq!(select_raw(&r, "0:msg").unwrap().data, b"hello\n");
        assert_eq!(select_raw(&r, "0:1").unwrap().identifier, "bin");
        assert!(select_raw(&r, "2:msg").is_err());
        assert!(select_raw(&r, "0:nope").is_err());
        assert!(select_raw(&r, "msg").is_err());
    }

    #[test]
    fn out_dir_is_safe() {
        let dir = tempfile::tempdir().unwrap();
        let written = write_out_dir(&resp(), dir.path()).unwrap();
        assert_eq!(std::fs::read(dir.path().join("0/msg")).unwrap(), b"hello\n");
        assert_eq!(std::fs::read(dir.path().join("1/.._x")).unwrap(), b"a");
        assert!(dir.path().join("timestamps.json").exists());
        assert_eq!(written.len(), 4);
    }
}
