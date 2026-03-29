use anyhow::Result;
use jj_lib::repo::Repo;

use crate::log::{BookmarkGraphNode, Log};

pub async fn submit(log: &Log, repo: &impl Repo, dry_run: bool) -> Result<()> {
    // TODO: fetch prs
    // TODO: dry run first

    // TODO: wet: take a snapshot

    // TODO: wet: abandon changes that have already been merged

    // TODO: wet: take a snapshot

    let bookmark_graph = log.bookmark_graph();
    for BookmarkGraphNode { bookmark, .. } in &bookmark_graph {
        // TODO: push bookmark
    }

    // TODO: wet: take a snapshot

    // TODO: wet: create prs if needed
    // let commit = repo.store().get_commit(&bookmark.commit_id).ok();
    // let title = commit
    //     .and_then(|c| c.description().lines().next().map(ToString::to_string))
    //     .unwrap_or_else(|| format!("Update {}", bookmark.name));
    //
    // steps.push(PlanStep {
    //     kind: PlanStepKind::EnsurePr {
    //         bookmark,
    //         title,
    //         base,
    //     },
    //     parent,
    // });

    // TODO: wet: for existing prs update base if needed

    // TODO: wet: create/update stack at the end of the PR description, marked with an HTML comment
    // at the start to make it easier to find and update, or create if the comment doesn't exist)

    Ok(())
}
