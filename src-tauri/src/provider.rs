use crate::model::{Harness, Session};
use std::borrow::Borrow;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, LazyLock};

const CODEX_PROVIDER_ID: &str = "codex";
const CLAUDE_CODE_PROVIDER_ID: &str = "claude_code";

/// Stable, provider-owned identifier. This intentionally remains separate
/// from `Harness` while persisted sessions and the frontend still use that
/// compatibility type.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderId(Arc<str>);

impl ProviderId {
    pub fn new(value: impl AsRef<str>) -> anyhow::Result<Self> {
        let value = value.as_ref();
        let mut chars = value.chars();
        let valid = chars.next().is_some_and(|first| first.is_ascii_lowercase())
            && chars.all(|ch| {
                ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-')
            });
        if !valid {
            anyhow::bail!(
                "provider id must start with a lowercase ASCII letter and contain only lowercase letters, digits, '_' or '-'"
            );
        }
        Ok(Self(Arc::from(value)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for ProviderId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Debug for ProviderId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ProviderId")
            .field(&self.as_str())
            .finish()
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ProviderId {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

pub fn codex_provider_id() -> ProviderId {
    ProviderId::new(CODEX_PROVIDER_ID).expect("built-in provider id is valid")
}

pub fn claude_code_provider_id() -> ProviderId {
    ProviderId::new(CLAUDE_CODE_PROVIDER_ID).expect("built-in provider id is valid")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub archived_sources: bool,
    pub session_index: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDescriptor {
    pub id: ProviderId,
    pub display_name: &'static str,
    pub capabilities: ProviderCapabilities,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderSourceKind {
    Live,
    Archived,
}

impl ProviderSourceKind {
    pub fn is_archived(self) -> bool {
        matches!(self, Self::Archived)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSource {
    provider_id: ProviderId,
    root: PathBuf,
    kind: ProviderSourceKind,
}

impl ProviderSource {
    pub fn new(provider_id: ProviderId, root: PathBuf, kind: ProviderSourceKind) -> Self {
        Self {
            provider_id,
            root,
            kind,
        }
    }

    pub fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn kind(&self) -> ProviderSourceKind {
        self.kind
    }
}

/// Immutable, pre-validated source ownership used by both bulk scanning and
/// incremental watching. Construction fails before any file access when a
/// provider is unknown or roots can assign one path to different owners.
#[derive(Debug, Clone)]
pub struct ProviderSourceSet {
    sources: Arc<[ProviderSource]>,
}

impl ProviderSourceSet {
    pub fn empty() -> Self {
        Self {
            sources: Arc::from(Vec::<ProviderSource>::new().into_boxed_slice()),
        }
    }

    pub fn try_new(sources: impl IntoIterator<Item = ProviderSource>) -> anyhow::Result<Self> {
        let registry = ProviderRegistry::builtin();
        let mut unique = Vec::<ProviderSource>::new();

        for source in sources {
            if source.root.as_os_str().is_empty() {
                anyhow::bail!("provider source root must not be empty");
            }
            let adapter = registry
                .adapter(source.provider_id())
                .ok_or_else(|| anyhow::anyhow!("unknown provider id '{}'", source.provider_id()))?;
            if source.kind().is_archived() && !adapter.descriptor().capabilities.archived_sources {
                anyhow::bail!(
                    "provider '{}' does not support archived sources",
                    source.provider_id()
                );
            }

            if let Some(existing) = unique.iter().find(|existing| {
                roots_overlap(existing.root(), source.root()) && !same_owner(existing, &source)
            }) {
                anyhow::bail!(
                    "overlapping provider roots {:?} and {:?} have ambiguous ownership",
                    existing.root(),
                    source.root()
                );
            }

            // An existing ancestor already covers this exact owner. Conversely,
            // replace covered descendants when the broader root arrives later.
            if unique.iter().any(|existing| {
                same_owner(existing, &source) && root_contains(existing.root(), source.root())
            }) {
                continue;
            }
            unique.retain(|existing| {
                !(same_owner(existing, &source) && root_contains(source.root(), existing.root()))
            });
            unique.push(source);
        }

        for (index, left) in unique.iter().enumerate() {
            for right in &unique[index + 1..] {
                if roots_overlap(left.root(), right.root()) && !same_owner(left, right) {
                    anyhow::bail!(
                        "overlapping provider roots {:?} and {:?} have ambiguous ownership",
                        left.root(),
                        right.root()
                    );
                }
            }
        }

        Ok(Self {
            sources: Arc::from(unique.into_boxed_slice()),
        })
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &ProviderSource> {
        self.sources.iter()
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Returns the most-specific configured root for a path. Validation
    /// guarantees overlapping matches all have the same provider and kind.
    pub fn resolve(&self, path: &Path) -> Option<&ProviderSource> {
        let path = strip_verbatim_prefix(path);
        self.sources
            .iter()
            .filter(|source| path.starts_with(strip_verbatim_prefix(source.root())))
            .max_by_key(|source| strip_verbatim_prefix(source.root()).components().count())
    }
}

pub trait IncrementalProviderParser: Send + Sync {
    fn parse_to_end(&mut self) -> anyhow::Result<bool>;
    fn session(&self) -> Option<&Session>;
}

impl IncrementalProviderParser for crate::parser::SessionParser {
    fn parse_to_end(&mut self) -> anyhow::Result<bool> {
        crate::parser::SessionParser::parse_to_end(self)
    }

    fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }
}

impl IncrementalProviderParser for crate::claude_parser::ClaudeSessionParser {
    fn parse_to_end(&mut self) -> anyhow::Result<bool> {
        crate::claude_parser::ClaudeSessionParser::parse_to_end(self)
    }

    fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }
}

pub trait ProviderAdapter: Send + Sync {
    fn descriptor(&self) -> &'static ProviderDescriptor;

    /// Pure path classification. Watchers also call this after removal, so an
    /// implementation must not require the file to exist or read its content.
    fn accepts_path(&self, path: &Path) -> bool;

    /// The scan cache is path-addressed for compatibility. Guard every hit
    /// against current ownership so config changes cannot serve a session
    /// parsed by the previous provider or live/archive classification.
    fn accepts_cached_session(&self, session: &Session, kind: ProviderSourceKind) -> bool;

    fn parse_file(&self, path: &Path, kind: ProviderSourceKind) -> anyhow::Result<Option<Session>>;

    fn incremental_parser(
        &self,
        path: PathBuf,
        kind: ProviderSourceKind,
    ) -> anyhow::Result<Box<dyn IncrementalProviderParser>>;
}

struct CodexAdapter;
struct ClaudeCodeAdapter;

static CODEX_DESCRIPTOR: LazyLock<ProviderDescriptor> = LazyLock::new(|| ProviderDescriptor {
    id: codex_provider_id(),
    display_name: "Codex",
    capabilities: ProviderCapabilities {
        archived_sources: true,
        session_index: true,
    },
});

static CLAUDE_CODE_DESCRIPTOR: LazyLock<ProviderDescriptor> =
    LazyLock::new(|| ProviderDescriptor {
        id: claude_code_provider_id(),
        display_name: "Claude Code",
        capabilities: ProviderCapabilities {
            archived_sources: false,
            session_index: false,
        },
    });

impl ProviderAdapter for CodexAdapter {
    fn descriptor(&self) -> &'static ProviderDescriptor {
        &CODEX_DESCRIPTOR
    }

    fn accepts_path(&self, path: &Path) -> bool {
        is_jsonl(path)
    }

    fn accepts_cached_session(&self, session: &Session, kind: ProviderSourceKind) -> bool {
        session.harness == Harness::Codex && session.archived == kind.is_archived()
    }

    fn parse_file(&self, path: &Path, kind: ProviderSourceKind) -> anyhow::Result<Option<Session>> {
        crate::parser::parse_file(path, kind.is_archived())
    }

    fn incremental_parser(
        &self,
        path: PathBuf,
        kind: ProviderSourceKind,
    ) -> anyhow::Result<Box<dyn IncrementalProviderParser>> {
        Ok(Box::new(crate::parser::SessionParser::new(
            path,
            kind.is_archived(),
        )))
    }
}

impl ProviderAdapter for ClaudeCodeAdapter {
    fn descriptor(&self) -> &'static ProviderDescriptor {
        &CLAUDE_CODE_DESCRIPTOR
    }

    fn accepts_path(&self, path: &Path) -> bool {
        is_jsonl(path)
    }

    fn accepts_cached_session(&self, session: &Session, kind: ProviderSourceKind) -> bool {
        session.harness == Harness::ClaudeCode && !kind.is_archived() && !session.archived
    }

    fn parse_file(&self, path: &Path, kind: ProviderSourceKind) -> anyhow::Result<Option<Session>> {
        ensure_live_only(self.descriptor(), kind)?;
        crate::claude_parser::parse_file(path)
    }

    fn incremental_parser(
        &self,
        path: PathBuf,
        kind: ProviderSourceKind,
    ) -> anyhow::Result<Box<dyn IncrementalProviderParser>> {
        ensure_live_only(self.descriptor(), kind)?;
        Ok(Box::new(crate::claude_parser::ClaudeSessionParser::new(
            path,
        )))
    }
}

fn ensure_live_only(
    descriptor: &ProviderDescriptor,
    kind: ProviderSourceKind,
) -> anyhow::Result<()> {
    if kind.is_archived() && !descriptor.capabilities.archived_sources {
        anyhow::bail!(
            "provider '{}' does not support archived sources",
            descriptor.id
        );
    }
    Ok(())
}

static CODEX_ADAPTER: CodexAdapter = CodexAdapter;
static CLAUDE_CODE_ADAPTER: ClaudeCodeAdapter = ClaudeCodeAdapter;
static BUILTIN_ADAPTERS: [&'static dyn ProviderAdapter; 2] = [&CODEX_ADAPTER, &CLAUDE_CODE_ADAPTER];

#[derive(Debug, Clone, Copy, Default)]
pub struct ProviderRegistry;

impl ProviderRegistry {
    pub fn builtin() -> &'static Self {
        static REGISTRY: ProviderRegistry = ProviderRegistry;
        &REGISTRY
    }

    pub fn adapters(&self) -> impl ExactSizeIterator<Item = &'static dyn ProviderAdapter> {
        BUILTIN_ADAPTERS.iter().copied()
    }

    pub fn descriptors(&self) -> impl ExactSizeIterator<Item = &'static ProviderDescriptor> {
        self.adapters().map(ProviderAdapter::descriptor)
    }

    pub fn adapter(&self, provider_id: &ProviderId) -> Option<&'static dyn ProviderAdapter> {
        self.adapters()
            .find(|adapter| &adapter.descriptor().id == provider_id)
    }
}

fn same_owner(left: &ProviderSource, right: &ProviderSource) -> bool {
    left.provider_id == right.provider_id && left.kind == right.kind
}

fn roots_overlap(left: &Path, right: &Path) -> bool {
    let left = strip_verbatim_prefix(left);
    let right = strip_verbatim_prefix(right);
    left.starts_with(right) || right.starts_with(left)
}

fn root_contains(parent: &Path, child: &Path) -> bool {
    strip_verbatim_prefix(child).starts_with(strip_verbatim_prefix(parent))
}

fn is_jsonl(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
}

fn strip_verbatim_prefix(path: &Path) -> &Path {
    if let Some(path) = path.to_str() {
        if let Some(stripped) = path.strip_prefix(r"\\?\") {
            return Path::new(stripped);
        }
    }
    path
}
