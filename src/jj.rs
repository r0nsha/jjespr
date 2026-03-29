use std::{path::Path, sync::Arc};

use anyhow::{Context, Result, bail};
use jj_lib::{
    backend::CommitId,
    commit::Commit,
    config::{ConfigLayer, ConfigNamePathBuf, ConfigSource, StackedConfig},
    dsl_util::{AliasDeclarationParser, AliasesMap},
    fileset::FilesetAliasesMap,
    git,
    graph::GraphNode,
    op_store::RefTarget,
    ref_name::{RefName, RemoteNameBuf},
    repo::{ReadonlyRepo, Repo, StoreFactories},
    repo_path::RepoPathUiConverter,
    revset::{
        self, Revset, RevsetAliasesMap, RevsetDiagnostics, RevsetExtensions, RevsetParseContext,
        RevsetWorkspaceContext, SymbolResolver, SymbolResolverExtension,
    },
    settings::UserSettings,
    str_util::{StringMatcher, StringPattern},
    time_util::DatePatternContext,
    view::View,
    workspace::{Workspace, default_working_copy_factories},
};

pub struct Jj {
    workspace: Workspace,
    settings: UserSettings,
    revset_aliases_map: RevsetAliasesMap,
    fileset_aliases_map: FilesetAliasesMap,
}

impl Jj {
    // Taken from https://github.com/jj-vcs/jj/blob/975ef64ca75677b834e4c14ffeb0c67aa487e623/cli/src/config/revsets.toml#L18
    const DEFAULT_TRUNK: &str = r#"latest(
        remote_bookmarks(exact:"main", exact:"origin") |
        remote_bookmarks(exact:"master", exact:"origin") |
        remote_bookmarks(exact:"trunk", exact:"origin") |
        remote_bookmarks(exact:"main", exact:"upstream") |
        remote_bookmarks(exact:"master", exact:"upstream") |
        remote_bookmarks(exact:"trunk", exact:"upstream") |
        root()
    )"#;

    pub fn new(workspace_path: &Path) -> Result<Self> {
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

        let mut revset_aliases_map = Self::load_revset_aliases_map(settings.config())?;
        if revset_aliases_map.get_function("trunk", 0).is_none() {
            revset_aliases_map
                .insert("trunk()", Self::DEFAULT_TRUNK)
                .expect("valid alias declaration");
        };

        let fileset_aliases_map = Self::load_fileset_aliases_map(settings.config())?;

        Ok(Self {
            workspace,
            settings,
            revset_aliases_map,
            fileset_aliases_map,
        })
    }

    fn load_revset_aliases_map(config: &StackedConfig) -> Result<RevsetAliasesMap> {
        Self::load_aliases_map("revset-aliases", config)
    }

    fn load_fileset_aliases_map(config: &StackedConfig) -> Result<FilesetAliasesMap> {
        Self::load_aliases_map("fileset-aliases", config)
    }

    fn load_aliases_map<P: Default + AliasDeclarationParser>(
        table_name: &str,
        config: &StackedConfig,
    ) -> Result<AliasesMap<P, String>> {
        let table_name = ConfigNamePathBuf::from_iter([table_name]);
        let mut aliases_map = AliasesMap::default();

        let Some(table) = config
            .layers()
            .iter()
            .find_map(|l| l.look_up_table(&table_name).ok().flatten())
        else {
            return Ok(aliases_map);
        };

        for (decl, item) in table.iter() {
            // We ignore invalid aliases, since JJ's cli already warns about them
            if let Some(v) = item.as_str() {
                let _ = aliases_map.insert(decl, v);
            }
        }

        Ok(aliases_map)
    }

    pub async fn repo(&self) -> Result<Arc<ReadonlyRepo>> {
        Ok(self.workspace.repo_loader().load_at_head().await?)
    }

    pub fn workspace_name(&self) -> &jj_lib::ref_name::WorkspaceName {
        self.workspace.workspace_name()
    }

    pub async fn get_working_copy_commit_id(&self) -> Result<CommitId> {
        let repo = self.repo().await?;
        let view = repo.view();
        let workspace_name = self.workspace_name();

        let Some(commit_id) = view.get_wc_commit_id(workspace_name) else {
            bail!(
                "no working copy commit found for workspace {:?}",
                workspace_name.as_str()
            )
        };

        Ok(commit_id.clone())
    }

    pub fn evaluate_revset_inner<'a>(
        &'a self,
        repo: &'a impl Repo,
        expr: &str,
    ) -> Result<Box<dyn Revset + 'a>> {
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
            aliases_map: &self.revset_aliases_map,
            fileset_aliases_map: &self.fileset_aliases_map,
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
            let symbol_resolver = SymbolResolver::new(repo, resolver_extensions);
            expr.resolve_user_expression(repo, &symbol_resolver)
                .context("failed to resolve revset")
        }?;

        let revset = resolved
            .evaluate(repo)
            .context("failed to evaluate revset")?;

        Ok(revset)
    }

    pub async fn evaluate_revset(&self, expr: &str) -> Result<Vec<Commit>> {
        let repo = self.repo().await?;
        let revset = self.evaluate_revset_inner(repo.as_ref(), expr)?;

        revset
            .iter()
            .map(|commit_id| {
                let commit_id = commit_id?;
                let commit = repo.store().get_commit(&commit_id)?;
                Ok(commit)
            })
            .collect::<Result<Vec<_>>>()
    }

    pub async fn evaluate_revset_graph(
        &self,
        expr: &str,
    ) -> Result<Vec<GraphNode<Commit, CommitId>>> {
        let repo = self.repo().await?;
        let revset = self.evaluate_revset_inner(repo.as_ref(), expr)?;

        let mut graph_nodes = Vec::new();

        for item in revset.iter_graph() {
            let (commit_id, edges) = item?;
            let commit = repo.store().get_commit(&commit_id)?;
            graph_nodes.push((commit, edges));
        }

        Ok(graph_nodes)
    }
}

#[derive(Debug, Clone)]
pub struct Bookmark {
    pub name: String,
    #[allow(unused)]
    pub commit_id: CommitId,
    #[allow(unused)]
    pub remote: Option<RemoteNameBuf>,
    pub synced: bool,
    pub is_base: bool,
}

impl Bookmark {
    pub fn from_name_and_target(
        view: &View,
        name: &RefName,
        target: &RefTarget,
        base: &str,
    ) -> Result<Self> {
        let Some(commit_id) = target.as_normal() else {
            bail!("bookmark {} is not a normal commit", name.as_str())
        };

        // Find the first remote bookmark that matches the local bookmark name, if any
        let bookmark_matcher = StringPattern::exact(name.as_str()).to_matcher();

        let mut remote: Option<RemoteNameBuf> = None;
        let mut synced = false;

        for (symbol, ref_) in view.remote_bookmarks_matching(&bookmark_matcher, &StringMatcher::All)
        {
            if symbol.remote == git::REMOTE_NAME_FOR_LOCAL_GIT_REPO {
                continue;
            }

            if remote.is_none() {
                remote = Some(symbol.remote.to_owned());
            }

            synced = synced || ref_.target.as_normal().is_some_and(|id| id == commit_id);
        }

        Ok(Self {
            name: name.as_str().to_string(),
            commit_id: commit_id.clone(),
            remote: None,
            synced,
            is_base: base == name.as_str(),
        })
    }
}
