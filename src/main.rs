use std::{path::Path, sync::Arc};

use anyhow::{Context, Result, anyhow, bail};
use jj_lib::{
    config::{ConfigLayer, ConfigNamePathBuf, ConfigSource, StackedConfig},
    object_id::ObjectId,
    ref_name::RemoteNameBuf,
    repo::{ReadonlyRepo, Repo, StoreFactories},
    revset::RevsetAliasesMap,
    settings::UserSettings,
    str_util::{StringMatcher, StringPattern},
    workspace::{Workspace, default_working_copy_factories},
};

// TODO: custom remotes
// TODO: custom base
fn main() -> Result<()> {
    // TODO: get all bookmarks
    let cwd = std::env::current_dir().context("Failed to get cwd")?;
    let jj = Jj::new(&cwd)?;
    let bookmarks = jj.bookmarks();
    dbg!(&bookmarks);
    dbg!(jj.trunk());
    Ok(())
}

struct Jj {
    workspace: Workspace,
    settings: UserSettings,
    revset_aliases: RevsetAliasesMap,
}

impl Jj {
    // Taken from jj/cli/src/config/revsets.toml
    const DEFAULT_TRUNK: &str = r#"latest(
        remote_bookmarks(exact:"main", exact:"origin") |
        remote_bookmarks(exact:"master", exact:"origin") |
        remote_bookmarks(exact:"trunk", exact:"origin") |
        remote_bookmarks(exact:"main", exact:"upstream") |
        remote_bookmarks(exact:"master", exact:"upstream") |
        remote_bookmarks(exact:"trunk", exact:"upstream") |
        root()
    )"#;

    fn new(workspace_path: &Path) -> Result<Self> {
        let mut config = StackedConfig::with_defaults();

        // User config
        if let Some(config_dir) = dirs::config_dir() {
            let config_file = config_dir.join("jj").join("config.toml");
            if config_file.exists() {
                config.load_file(ConfigSource::User, config_file)?;
            }
        } else {
            let mut user_layer = ConfigLayer::empty(ConfigSource::Default);
            user_layer.set_value("user.name", "jjespr")?;
            user_layer.set_value("user.email", "jjespr@localhost")?;
            config.add_layer(user_layer);
        }

        // Repo config
        let repo_path = workspace_path.join(".jj").join("repo").join("config.toml");
        if repo_path.exists() {
            config.load_file(ConfigSource::Repo, repo_path)?;
        }

        let settings = UserSettings::from_config(config)?;

        let workspace = Workspace::load(
            &settings,
            workspace_path,
            &StoreFactories::default(),
            &default_working_copy_factories(),
        )?;

        let mut revset_aliases = Self::load_revset_aliases(settings.config())?;
        if revset_aliases.get_function("trunk", 0).is_none() {
            revset_aliases
                .insert("trunk()", Self::DEFAULT_TRUNK)
                .expect("valid alias declaration");
        };

        Ok(Self {
            workspace,
            settings,
            revset_aliases,
        })
    }

    pub fn load_revset_aliases(config: &StackedConfig) -> Result<RevsetAliasesMap> {
        let table_name = ConfigNamePathBuf::from_iter(["revset-aliases"]);
        let mut aliases_map = RevsetAliasesMap::new();

        let Some(table) = config
            .layers()
            .iter()
            .find_map(|l| l.look_up_table(&table_name).ok().flatten())
        else {
            return Ok(aliases_map);
        };

        for (decl, item) in table.iter() {
            // We ignore invalid revset aliases, since JJ's cli already warns about them
            if let Some(v) = item.as_str() {
                let _ = aliases_map.insert(decl, v);
            }
        }

        Ok(aliases_map)
    }

    fn repo(&self) -> Result<Arc<ReadonlyRepo>> {
        Ok(self.workspace.repo_loader().load_at_head()?)
    }

    fn trunk(&self) -> &str {
        let (_, _, defn) = self
            .revset_aliases
            .get_function("trunk", 0)
            .expect("trunk() alias is defined");
        defn
    }

    fn bookmarks(&self) -> Result<Vec<Bookmark>> {
        let repo = self.repo()?;
        let view = repo.view();

        let mut bookmarks: Vec<Bookmark> = vec![];

        for (name, target) in view.local_bookmarks() {
            let Some(commit_id) = target.as_normal() else {
                continue;
            };

            let Ok(commit) = repo.store().get_commit(commit_id) else {
                eprintln!(
                    "Warning: Failed to get commit {commit_id} for bookmark {}",
                    name.as_str()
                );
                continue;
            };

            // Find the first remote bookmark that matches the local bookmark name, if any
            let bookmark_matcher = StringPattern::exact(name.as_str()).to_matcher();

            let mut remote: Option<RemoteNameBuf> = None;
            let mut synced = false;

            for (symbol, ref_) in
                view.remote_bookmarks_matching(&bookmark_matcher, &StringMatcher::All)
            {
                let remote_name = symbol.remote.as_str();

                if remote_name == "git" {
                    continue;
                }

                if remote.is_none() {
                    remote = Some(symbol.remote.to_owned());
                }

                synced = synced || ref_.target.as_normal().is_some_and(|id| id == commit_id);
            }

            bookmarks.push(Bookmark {
                name: name.as_str().to_string(),
                change_id: commit.change_id().hex(),
                commit_id: commit_id.hex(),
                remote,
                synced,
            });
        }

        Ok(bookmarks)
    }

    // pub fn maybe_set_repository_level_trunk_alias(
    //     ui: &Ui,
    //     git_repo: &gix::Repository,
    //     config_env: &ConfigEnv,
    // ) -> Result<(), CommandError> {
    //     // Try "upstream" first, then fall back to "origin"
    //     for remote in ["upstream", "origin"] {
    //         let ref_name = format!("refs/remotes/{remote}/HEAD");
    //         if let Some(reference) = git_repo
    //             .try_find_reference(&ref_name)
    //             .map_err(internal_error)?
    //         {
    //             // Found a HEAD reference for this remote. Even if we can't parse it,
    //             // we should stop here and not try other remotes because it doesn't
    //             // really make sense if "origin" were to be set as the default if we
    //             // know "upstream" exists.
    //             if let Some(reference_name) = reference.target().try_name()
    //                 && let Some((GitRefKind::Bookmark, symbol)) =
    //                     str::from_utf8(reference_name.as_bstr())
    //                         .ok()
    //                         .and_then(|name| parse_git_ref(name.as_ref()))
    //             {
    //                 // TODO: Can we assume the symbolic target points to the same remote?
    //                 let symbol = symbol.name.to_remote_symbol(remote.as_ref());
    //                 write_repository_level_trunk_alias(ui, config_env, symbol)?;
    //             }
    //             return Ok(());
    //         }
    //     }
    //
    //     Ok(())
    // }

    // fn evaluate_revset(&self, expr: &str) -> Result<()> {
    //     let repo = self.repo()?;
    //     let extensions = RevsetExtensions::new();
    //     let mut aliases = RevsetAliasesMap::default();
    //
    //     // // Define trunk() alias - checks remote HEAD first, then falls back to jj's default
    //     // let trunk_alias = Self::compute_trunk_alias(&repo);
    //     // aliases
    //     //     .insert("trunk()", trunk_alias)
    //     //     .expect("trunk() alias declaration is valid");
    //     //
    //     // let date_context = jj_lib::time_util::DatePatternContext::Local(chrono::Local::now());
    //     //
    //     // // Create workspace context for trunk() resolution
    //     // let workspace_root = self.workspace.workspace_root().to_path_buf();
    //     // let path_converter = RepoPathUiConverter::Fs {
    //     //     cwd: workspace_root.clone(),
    //     //     base: workspace_root,
    //     // };
    //     // let workspace_name = self.workspace.workspace_name();
    //     // let workspace_ctx = RevsetWorkspaceContext {
    //     //     path_converter: &path_converter,
    //     //     workspace_name,
    //     // };
    //     //
    //     // let context = RevsetParseContext {
    //     //     aliases_map: &aliases,
    //     //     local_variables: std::collections::HashMap::new(),
    //     //     user_email: self.settings.user_email(),
    //     //     date_pattern_context: date_context,
    //     //     default_ignored_remote: Some(git::REMOTE_NAME_FOR_LOCAL_GIT_REPO),
    //     //     use_glob_by_default: false,
    //     //     extensions: &extensions,
    //     //     workspace: Some(workspace_ctx),
    //     // };
    //     //
    //     // let mut diagnostics = RevsetDiagnostics::new();
    //     // let expression = parse(&mut diagnostics, expr, &context)
    //     //     .map_err(|e| Error::Parse(format!("Failed to parse revset: {e}")))?;
    //     //
    //     // let empty_extensions: &[Box<dyn SymbolResolverExtension>] = &[];
    //     // let symbol_resolver = SymbolResolver::new(repo.as_ref(), empty_extensions);
    //     // let resolved = expression
    //     //     .resolve_user_expression(repo.as_ref(), &symbol_resolver)
    //     //     .map_err(|e| Error::Revset(format!("Failed to resolve revset: {e}")))?;
    //     //
    //     // let revset = resolved
    //     //     .evaluate(repo.as_ref())
    //     //     .map_err(|e| Error::Revset(format!("Failed to evaluate revset: {e}")))?;
    //     //
    //     // let mut entries = Vec::new();
    //     // for commit_id in revset.iter() {
    //     //     let commit_id =
    //     //         commit_id.map_err(|e| Error::Revset(format!("Failed to iterate revset: {e}")))?;
    //     //     let commit = repo
    //     //         .store()
    //     //         .get_commit(&commit_id)
    //     //         .map_err(|e| Error::Workspace(format!("Failed to get commit: {e}")))?;
    //     //
    //     //     entries.push(Self::commit_to_log_entry(&repo, &commit));
    //     // }
    //     //
    //     // Ok(entries)
    //     Ok(())
    // }
}

#[derive(Debug)]
struct Bookmark {
    name: String,
    change_id: String,
    commit_id: String,
    remote: Option<RemoteNameBuf>,
    synced: bool,
}
