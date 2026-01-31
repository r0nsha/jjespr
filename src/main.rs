use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use jj_lib::{
    commit::Commit,
    config::{ConfigLayer, ConfigNamePathBuf, ConfigSource, StackedConfig},
    git,
    object_id::ObjectId,
    ref_name::RemoteNameBuf,
    repo::{ReadonlyRepo, Repo, StoreFactories},
    repo_path::RepoPathUiConverter,
    revset::{
        self, RevsetAliasesMap, RevsetDiagnostics, RevsetExtensions, RevsetParseContext,
        RevsetWorkspaceContext, SymbolResolver, SymbolResolverExtension,
    },
    settings::UserSettings,
    str_util::{StringMatcher, StringPattern},
    time_util::DatePatternContext,
    workspace::{Workspace, default_working_copy_factories},
};

#[derive(Parser, Debug)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Sets the JJ workspace root
    #[arg(short, long, value_name = "DIR")]
    path: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Submit(SubmitArgs),
}

#[derive(Args, Debug)]
struct SubmitArgs {
    /// Optional bookmark name
    bookmark: Option<String>,
}

// TODO: custom remotes
// TODO: custom base
fn main() -> Result<()> {
    let args = Cli::parse();

    let root = if let Some(path) = args.path {
        path
    } else {
        std::env::current_dir().context("failed to get cwd")?
    };

    match args.command {
        Commands::Submit(args) => cmd_submit(root, args),
    }
}

fn submit(root: PathBuf, args: SubmitArgs) -> Result<()> {
    let jj = Jj::new(&root)?;
    let target_bookmark = if let Some(bookmark) = args.bookmark {
        // TODO: validate target_bookmark
        let repo = jj.repo()?;
        let view = repo.view();
        view.local_bookmarks()
            .find(|(name, _)| name.as_str() == bookmark)
            .with_context(|| format!("bookmark {bookmark} doesn't exist"))?;
        bookmark
    } else {
        // TODO
        todo!("interactive stack picker")
    };
    let commits = jj.evaluate_revset(&format!("trunk()::{target_bookmark}"));
    dbg!(&commits);
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
                if symbol.remote == git::REMOTE_NAME_FOR_LOCAL_GIT_REPO {
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

    fn evaluate_revset(&self, expr: &str) -> Result<Vec<Commit>> {
        let repo = self.repo()?;
        let extensions = RevsetExtensions::new();

        let date_context = DatePatternContext::Local(chrono::Local::now());

        // Create workspace context for trunk() resolution
        let workspace_root = self.workspace.workspace_root().to_path_buf();
        let path_converter = RepoPathUiConverter::Fs {
            cwd: workspace_root.clone(),
            base: workspace_root,
        };
        let workspace_name = self.workspace.workspace_name();
        let workspace_ctx = RevsetWorkspaceContext {
            path_converter: &path_converter,
            workspace_name,
        };

        let context = RevsetParseContext {
            aliases_map: &self.revset_aliases,
            local_variables: std::collections::HashMap::new(),
            user_email: self.settings.user_email(),
            date_pattern_context: date_context,
            default_ignored_remote: Some(git::REMOTE_NAME_FOR_LOCAL_GIT_REPO),
            use_glob_by_default: false,
            extensions: &extensions,
            workspace: Some(workspace_ctx),
        };

        let mut diagnostics = RevsetDiagnostics::new();
        let Ok(expr) = revset::parse(&mut diagnostics, expr, &context) else {
            for diag in diagnostics.iter() {
                eprintln!("{diag}");
            }
            bail!("failed to parse revset");
        };

        let resolved = {
            let resolver_extensions: &[Box<dyn SymbolResolverExtension>] = &[];
            let symbol_resolver = SymbolResolver::new(repo.as_ref(), resolver_extensions);
            expr.resolve_user_expression(repo.as_ref(), &symbol_resolver)
                .context("failed to resolve revset")
        }?;

        let revset = resolved
            .evaluate(repo.as_ref())
            .context("failed to evaluate revset")?;

        revset
            .iter()
            .map(|commit_id| {
                let commit_id = commit_id?;
                let commit = repo.store().get_commit(&commit_id)?;
                Ok(commit)
            })
            .collect::<Result<Vec<_>>>()
    }
}

#[derive(Debug)]
struct Bookmark {
    name: String,
    change_id: String,
    commit_id: String,
    remote: Option<RemoteNameBuf>,
    synced: bool,
}
