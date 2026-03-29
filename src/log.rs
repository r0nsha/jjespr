use std::collections::HashMap;

use anyhow::Result;
use jj_lib::{
    backend::CommitId,
    commit::Commit,
    graph::{GraphEdge, GraphEdgeType, GraphNode},
    id_prefix::IdPrefixIndex,
    object_id::ObjectId,
    repo::Repo,
};
use owo_colors::OwoColorize;
use renderdag::{Ancestor, GraphRowRenderer, Renderer};

use crate::jj::{Bookmark, Jj};

const SHORT_ID_LEN: usize = 8;

pub struct Log {
    graph: Vec<GraphNode<Commit, CommitId>>,
    bookmarks: Vec<Bookmark>,
    commit_bookmarks: HashMap<CommitId, Vec<usize>>,
}

impl Log {
    pub async fn new(jj: &Jj, base_revset: &str) -> Result<Self> {
        let base_commits = jj.evaluate_revset(base_revset).await?;

        if base_commits.len() != 1 {
            anyhow::bail!("base revset `{base_revset}` must resolve to exactly one commit")
        }

        let repo = jj.repo().await?;
        let view = repo.view();

        let base_bookmarks: Vec<_> = view
            .local_bookmarks_for_commit(base_commits[0].id())
            .map(|(name, _)| name.as_str().to_string())
            .collect();

        if base_bookmarks.is_empty() {
            anyhow::bail!(
                "base commit `{}` has no bookmarks",
                base_commits[0].id().hex()
            )
        };

        if base_bookmarks.len() > 1 {
            anyhow::bail!(
                "base commit `{}` has multiple bookmarks, base commit must have exactly one",
                base_commits[0].id().hex()
            )
        }

        let base_bookmark = &base_bookmarks[0];

        // Get working copy commit
        let working_copy_id = jj.get_working_copy_commit_id().await?;

        // Collect all local bookmarks with their commit IDs
        let mut bookmark_distances: Vec<(String, usize)> = Vec::new();
        
        for (name, target) in view.local_bookmarks() {
            let Some(bookmark_commit_id) = target.as_normal() else {
                continue;
            };
            
            let bookmark_name = name.as_str().to_string();
            
            // Check if this bookmark is an ancestor of the working copy
            // We use the revset: bookmark_commit..working_copy
            let ancestor_check_revset = format!("{}..{}", bookmark_commit_id.hex(), working_copy_id.hex());
            
            match jj.evaluate_revset(&ancestor_check_revset).await {
                Ok(commits) => {
                    // If the bookmark is an ancestor, the distance is the number of commits between them
                    // If working copy IS the bookmark, distance is 0
                    let distance = if bookmark_commit_id == &working_copy_id {
                        0
                    } else {
                        commits.len()
                    };
                    bookmark_distances.push((bookmark_name, distance));
                }
                Err(_) => {
                    // Bookmark is not an ancestor of working copy, skip it
                    continue;
                }
            }
        }

        // Find the closest bookmark (minimum distance)
        let closest_bookmark = if let Some((closest, _)) = bookmark_distances.iter().min_by_key(|(_, distance)| *distance) {
            closest.clone()
        } else {
            // No bookmarks are ancestors of working copy, fall back to base
            base_bookmark.clone()
        };

        // Build the revset based on closest bookmark
        let revset_expr = if closest_bookmark == *base_bookmark {
            // Closest bookmark is the base, show all branches
            format!("{base_revset}::")
        } else {
            // Check if the closest bookmark has any descendant bookmarks
            let descendant_check_revset = format!("bookmarks({closest_bookmark}):: & bookmarks()");
            let descendant_bookmarks = jj.evaluate_revset(&descendant_check_revset).await?;
            
            // Get the commit ID of the closest bookmark
            let closest_bookmark_commit = jj.evaluate_revset(&closest_bookmark).await?;
            let has_other_descendant_bookmarks = descendant_bookmarks.len() > 1 || 
                (descendant_bookmarks.len() == 1 && descendant_bookmarks[0].id() != closest_bookmark_commit[0].id());
            
            if has_other_descendant_bookmarks {
                // The closest bookmark has descendant branches, show all
                format!("{base_revset}::")
            } else {
                // Show all bookmarks that are ancestors of the closest bookmark
                format!("bookmarks() & ::bookmarks({closest_bookmark})")
            }
        };

        let graph = jj
            .evaluate_revset_graph(&revset_expr)
            .await?;

        let mut this = Self {
            graph,
            bookmarks: Vec::new(),
            commit_bookmarks: HashMap::new(),
        };

        for (commit, _) in &this.graph {
            let id = commit.id();
            let start_idx = this.bookmarks.len();
            let bookmarks = view
                .local_bookmarks_for_commit(id)
                .map(|(name, target)| {
                    Bookmark::from_name_and_target(view, name, target, base_bookmark)
                })
                .collect::<Result<Vec<_>>>()?;
            let end_idx = start_idx + bookmarks.len();
            this.bookmarks.extend(bookmarks);

            let bookmark_indices = (start_idx..end_idx).collect::<Vec<_>>();
            this.commit_bookmarks.insert(id.clone(), bookmark_indices);
        }

        Ok(this)
    }

    fn bookmarks_for_commit(&self, commit_id: &CommitId) -> Vec<&Bookmark> {
        self.commit_bookmarks
            .get(commit_id)
            .map(|indices| indices.iter().map(|i| &self.bookmarks[*i]).collect())
            .unwrap_or_default()
    }

    pub fn display<'a, R: Repo>(&self, repo: &'a R) -> DisplayLog<'a> {
        let mut entries: Vec<DisplayLogEntry> = Vec::new();
        let mut elided_group: Vec<(CommitId, Vec<GraphEdge<CommitId>>)> = Vec::new();

        for (commit, edges) in &self.graph {
            let bookmarks: Vec<_> = self
                .bookmarks_for_commit(commit.id())
                .into_iter()
                .cloned()
                .collect();

            if bookmarks.is_empty() {
                elided_group.push((commit.id().clone(), edges.clone()));
            } else {
                if let Some((_, last_edges)) = elided_group.last() {
                    let edges = last_edges.clone();
                    let commit_ids: Vec<_> = elided_group.drain(..).map(|(id, _)| id).collect();
                    entries.push(DisplayLogEntry::Elided { edges, commit_ids });
                }

                entries.push(DisplayLogEntry::Commit {
                    commit: commit.clone(),
                    edges: edges.clone(),
                    bookmarks,
                });
            }
        }

        // remove leading and trailing elided revisions
        let start = entries
            .iter()
            .position(|e| !matches!(e, DisplayLogEntry::Elided { .. }))
            .unwrap_or(entries.len());

        let end = entries
            .iter()
            .rposition(|e| !matches!(e, DisplayLogEntry::Elided { .. }))
            .map(|i| i + 1)
            .unwrap_or(0);

        DisplayLog {
            repo,
            entries: entries[start..end.max(start)].to_vec(),
            id_prefix_index: IdPrefixIndex::empty(),
        }
    }
}

#[derive(Debug, Clone)]
enum DisplayLogEntry {
    Commit {
        commit: Commit,
        edges: Vec<GraphEdge<CommitId>>,
        bookmarks: Vec<Bookmark>,
    },
    Elided {
        edges: Vec<GraphEdge<CommitId>>,
        commit_ids: Vec<CommitId>,
    },
}

pub struct DisplayLog<'a> {
    repo: &'a dyn Repo,
    entries: Vec<DisplayLogEntry>,
    id_prefix_index: IdPrefixIndex<'a>,
}

impl std::fmt::Display for DisplayLog<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.entries.is_empty() {
            return writeln!(f, "~");
        }

        let mut commit_to_entry_idx: HashMap<CommitId, usize> = HashMap::new();
        for (idx, entry) in self.entries.iter().enumerate() {
            match entry {
                DisplayLogEntry::Commit { commit, .. } => {
                    commit_to_entry_idx.insert(commit.id().clone(), idx);
                }
                DisplayLogEntry::Elided { commit_ids, .. } => {
                    for id in commit_ids {
                        commit_to_entry_idx.insert(id.clone(), idx);
                    }
                }
            }
        }

        let builder = GraphRowRenderer::new().output().with_min_row_height(0);
        let mut renderer = builder.build_box_drawing();

        for (idx, entry) in self.entries.iter().enumerate() {
            let (glyph, message, edges) = match entry {
                DisplayLogEntry::Commit {
                    commit,
                    edges,
                    bookmarks,
                } => {
                    let glyph = if bookmarks.iter().any(|b| b.is_base) {
                        "◆"
                    } else {
                        "○"
                    };
                    let message = self.format_change_message(commit, bookmarks);
                    (glyph.to_string(), message, edges)
                }
                DisplayLogEntry::Elided { edges, commit_ids } => {
                    let message = format!(
                        "({} elided revision{})",
                        commit_ids.len(),
                        if commit_ids.len() == 1 { "" } else { "s" }
                    );
                    (
                        "~".bright_black().to_string(),
                        message.bright_black().to_string(),
                        edges,
                    )
                }
            };

            let ancestors: Vec<_> = edges
                .iter()
                .map(|edge| match commit_to_entry_idx.get(&edge.target) {
                    Some(&parent_idx) if parent_idx != idx => match edge.edge_type {
                        GraphEdgeType::Direct => Ancestor::Parent(parent_idx),
                        GraphEdgeType::Indirect => Ancestor::Ancestor(parent_idx),
                        GraphEdgeType::Missing => Ancestor::Anonymous,
                    },
                    _ => Ancestor::Anonymous,
                })
                .collect();

            let row = renderer.next_row(idx, ancestors, glyph, message);
            f.write_str(&row)?;
        }

        Ok(())
    }
}

impl DisplayLog<'_> {
    fn format_change_message(&self, commit: &Commit, bookmarks: &[Bookmark]) -> String {
        let mut parts = Vec::new();

        if !bookmarks.is_empty() {
            let bookmark_str = bookmarks
                .iter()
                .map(|b| {
                    let name = if b.is_base {
                        b.name.cyan().to_string()
                    } else {
                        b.name.green().bold().to_string()
                    };
                    if b.synced { name } else { format!("{}*", name) }
                })
                .collect::<Vec<_>>()
                .join(" ");
            parts.push(bookmark_str);
        }

        let change_id_hex = commit.change_id().hex();
        let change_id_prefix_len = self
            .id_prefix_index
            .shortest_change_prefix_len(self.repo, commit.change_id())
            .unwrap_or(SHORT_ID_LEN);
        let (prefix, rest) = change_id_hex.split_at(change_id_prefix_len.min(change_id_hex.len()));
        let rest = if change_id_prefix_len >= SHORT_ID_LEN {
            ""
        } else {
            &rest[..SHORT_ID_LEN.saturating_sub(change_id_prefix_len)]
        };
        let change_id_str = format!("{}{}", prefix.magenta(), rest.bright_black());
        parts.push(change_id_str);

        let author = commit.author();
        parts.push(author.email.yellow().to_string());
        parts.push(
            author
                .timestamp
                .to_datetime()
                .expect("valid datetime")
                .format("%Y-%m-%d %H:%M:%S")
                .cyan()
                .to_string(),
        );

        let commit_id_hex = commit.id().hex();
        let commit_id_prefix_len = self
            .id_prefix_index
            .shortest_commit_prefix_len(self.repo, commit.id())
            .unwrap_or(SHORT_ID_LEN);
        let (prefix, rest) = commit_id_hex.split_at(commit_id_prefix_len.min(commit_id_hex.len()));
        let rest = if commit_id_prefix_len >= SHORT_ID_LEN {
            ""
        } else {
            &rest[..SHORT_ID_LEN.saturating_sub(commit_id_prefix_len)]
        };
        let commit_id_str = format!("{}{}", prefix.cyan(), rest.bright_black());
        parts.push(commit_id_str);

        let desc = commit
            .description()
            .lines()
            .next()
            .unwrap_or("(no description set)");

        format!("{}\n{}", parts.join(" "), desc)
    }
}
