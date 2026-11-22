//! The mobile apps' native task replica for the relay's hosted HTTP contract.
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use zerus_replica_ffi::{TaskPosition, TaskStage, TaskStore, TaskTarget};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [root, project, name, add @ ..] = args.as_slice() else {
        return Err(io::Error::other("expected ROOT PROJECT NAME [TASK TEXT]").into());
    };
    let store = TaskStore::new(root.clone(), project.clone(), name.clone());
    match add {
        [] => {}
        [text] => {
            store.add(
                text.clone(),
                TaskStage::Todo,
                TaskTarget::AnySession,
                TaskPosition::End,
            )?;
        }
        _ => return Err(io::Error::other("expected at most one task to file").into()),
    }

    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let exchange = store.clone().exchange();
    let mut changed = 0;
    while let Some(request) = exchange.next_request()? {
        let request: Value = serde_json::from_str(&request)?;
        writeln!(output, "{}", json!({"request": request}))?;
        output.flush()?;
        let mut reply = String::new();
        input.read_line(&mut reply)?;
        let progress = exchange.accept(reply)?;
        if !progress.refused_here.is_empty()
            || !progress.refused_there.is_empty()
            || !progress.unsent.is_empty()
        {
            return Err(
                io::Error::other(format!("task exchange refused an update: {progress:?}")).into(),
            );
        }
        changed += progress.changed;
    }

    let listing = store.list()?;
    if !listing.damaged.is_empty() {
        return Err(io::Error::other(format!("damaged phone tasks: {:?}", listing.damaged)).into());
    }
    let tasks: Vec<_> = listing.tasks.into_iter().map(|task| {
        json!({"uuid": task.uuid, "text": task.text, "writable": task.writable, "schema": task.schema})
    }).collect();
    writeln!(
        output,
        "{}",
        json!({"digest": store.live_digest()?, "changed": changed, "tasks": tasks})
    )?;
    Ok(())
}
