use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::cli::{Globals, InvokeArgs};
use crate::client::{Client, InvokeTarget, Item, Set, decode_invoke_response, invoke_document};
use crate::config::Settings;
use crate::output;
use crate::parse::{self, InputItemArg, InputValue};

pub fn run(globals: &Globals, args: InvokeArgs) -> Result<()> {
    let target = match (&args.name, &args.composition) {
        (Some(n), None) => InvokeTarget::Name(n.clone()),
        (None, Some(c)) => InvokeTarget::Composition(super::read_text_arg(c)?),
        _ => bail!("give either a NAME or --composition"),
    };
    let sets = match &args.input_dir {
        Some(dir) => sets_from_dir(dir)?,
        None => sets_from_inputs(&args.inputs)?,
    };

    let settings = Settings::load(globals)?;
    let path = if args.no_cache { &settings.cold_path } else { &settings.hot_path };
    let client = Client::new(&settings)?;
    let body = client.post_bson(path, &invoke_document(&target, &sets))?;
    let resp = decode_invoke_response(&body)?;

    if let Some(spec) = &args.raw {
        output::write_stdout(&output::select_raw(&resp, spec)?.data)
    } else if args.json {
        println!("{}", serde_json::to_string_pretty(&output::to_json(&resp))?);
        Ok(())
    } else if let Some(dir) = &args.out_dir {
        let written = output::write_out_dir(&resp, dir)?;
        println!("wrote {} file(s) to {}", written.len(), dir.display());
        Ok(())
    } else {
        print!("{}", output::summary(&resp));
        Ok(())
    }
}

/// Groups `--input` items into sets, in the order each set first appears. Items keep the order
/// given; a missing key defaults to the item's position within its set.
pub fn sets_from_inputs(inputs: &[InputItemArg]) -> Result<Vec<Set>> {
    let mut names: Vec<&str> = Vec::new();
    let mut sets: Vec<Set> = Vec::new();
    for input in inputs {
        let idx = match names.iter().position(|n| *n == input.set) {
            Some(i) => i,
            None => {
                names.push(&input.set);
                sets.push(Set::default());
                sets.len() - 1
            }
        };
        let set = &mut sets[idx];
        let data = match &input.value {
            InputValue::File(p) => super::read_bytes(p)?,
            InputValue::Literal(s) => s.clone().into_bytes(),
        };
        let key = input.key.unwrap_or(set.items.len() as i64);
        set.items.push(Item { identifier: input.item.clone(), key, data });
    }
    Ok(sets)
}

fn sorted_entries(dir: &Path) -> Result<Vec<std::fs::DirEntry>> {
    let mut entries = std::fs::read_dir(dir)
        .with_context(|| format!("cannot read {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("cannot read {}", dir.display()))?;
    // Hidden files (`.DS_Store`, editor swap files) are never inputs.
    entries.retain(|e| !e.file_name().to_string_lossy().starts_with('.'));
    entries.sort_by_key(|e| e.file_name());
    Ok(entries)
}

/// Each subdirectory of `dir` is a set (sorted by name); each file in it is an item named
/// `<ITEM>[#KEY]` (sorted by file name). Hidden entries are skipped.
pub fn sets_from_dir(dir: &Path) -> Result<Vec<Set>> {
    let mut sets = Vec::new();
    for set_entry in sorted_entries(dir)? {
        let set_path = set_entry.path();
        if !set_path.is_dir() {
            bail!(
                "{} is not a directory; --input-dir must contain only one subdirectory per set",
                set_path.display()
            );
        }
        let mut set = Set::default();
        for item_entry in sorted_entries(&set_path)? {
            let item_path = item_entry.path();
            if !item_path.is_file() {
                bail!("{} is not a file", item_path.display());
            }
            let file_name = item_entry.file_name();
            let file_name = file_name
                .to_str()
                .with_context(|| format!("{} is not valid UTF-8", item_path.display()))?;
            let (identifier, key) = parse::item_and_key(file_name)
                .map_err(|e| anyhow::anyhow!("{}: {e}", item_path.display()))?;
            let key = key.unwrap_or(set.items.len() as i64);
            set.items.push(Item { identifier, key, data: super::read_bytes(&item_path)? });
        }
        sets.push(set);
    }
    Ok(sets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_grouped_in_first_appearance_order() {
        let inputs: Vec<InputItemArg> = ["b:x=1", "a:y#9=2", "b:z=3", "a:w=4"]
            .iter()
            .map(|s| parse::input_item(s).unwrap())
            .collect();
        let sets = sets_from_inputs(&inputs).unwrap();
        assert_eq!(sets.len(), 2);
        let ids: Vec<(&str, i64)> = sets[0].items.iter().map(|i| (i.identifier.as_str(), i.key)).collect();
        assert_eq!(ids, [("x", 0), ("z", 1)]);
        let ids: Vec<(&str, i64)> = sets[1].items.iter().map(|i| (i.identifier.as_str(), i.key)).collect();
        assert_eq!(ids, [("y", 9), ("w", 1)]);
        assert_eq!(sets[1].items[1].data, b"4");
    }

    #[test]
    fn dir_layout() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("b_set")).unwrap();
        std::fs::create_dir_all(d.path().join("a_set")).unwrap();
        std::fs::write(d.path().join("a_set/second#5"), b"2").unwrap();
        std::fs::write(d.path().join("a_set/first"), b"1").unwrap();
        std::fs::write(d.path().join("a_set/.DS_Store"), b"").unwrap();
        std::fs::write(d.path().join("b_set/only"), b"x").unwrap();
        let sets = sets_from_dir(d.path()).unwrap();
        assert_eq!(sets.len(), 2);
        assert_eq!(sets[0].items[0], Item { identifier: "first".into(), key: 0, data: b"1".to_vec() });
        assert_eq!(sets[0].items[1], Item { identifier: "second".into(), key: 5, data: b"2".to_vec() });
        assert_eq!(sets[1].items[0].identifier, "only");

        std::fs::write(d.path().join("stray.txt"), b"").unwrap();
        assert!(sets_from_dir(d.path()).is_err());
    }
}
