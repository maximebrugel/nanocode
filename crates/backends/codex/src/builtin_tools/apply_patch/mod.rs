use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use nanocode_process_contracts::FileHost;
use nanocode_tools::ToolDefinition;
use serde_json::json;

use super::{StandardTool, Tool, ToolContext, ToolInput, ToolOutput, ToolResult};

mod parser;
mod seek_sequence;
mod streaming_parser;

use parser::{Hunk, UpdateFileChunk, parse_patch};

static LENIENT_MATCHING: AtomicBool = AtomicBool::new(false);

pub fn set_lenient_matching(enabled: bool) {
    LENIENT_MATCHING.store(enabled, Ordering::Relaxed);
}

fn lenient(context: ToolContext<'_>) -> bool {
    context.tool_settings().lenient_patches || LENIENT_MATCHING.load(Ordering::Relaxed)
}

#[cfg(not(target_family = "wasm"))]
pub(crate) struct ApplyPatchHandler {
    workspace: PathBuf,
}

#[cfg(not(target_family = "wasm"))]
impl ApplyPatchHandler {
    pub(crate) const fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

#[cfg(not(target_family = "wasm"))]
impl Tool for ApplyPatchHandler {
    fn definition(&self) -> ToolDefinition {
        StandardTool::ApplyPatch.definition()
    }

    fn execute<'life0, 'life1, 'async_trait>(
        &'life0 self,
        input: ToolInput,
        context: ToolContext<'life1>,
    ) -> std::pin::Pin<Box<dyn Future<Output = ToolResult> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let output: ToolResult = {
                let input = patch_input(input)?;
                let workspace = self.workspace.clone();
                let lenient = lenient(context);
                Ok(
                    match nanocode_platform::spawn_blocking(move || {
                        apply_in(&input, &workspace, lenient)
                    })
                    .await
                    {
                        Ok(Ok(output)) => {
                            ToolOutput::text(output).with_structured_result(json!({}))
                        }
                        Ok(Err(error)) => {
                            ToolOutput::error(format!("apply_patch verification failed: {error}"))
                        }
                        Err(error) => {
                            ToolOutput::error(format!("apply_patch task failed: {error}"))
                        }
                    },
                )
            };
            output
        })
    }
}

pub(crate) struct HostApplyPatchHandler {
    workspace: PathBuf,
    files: std::sync::Arc<dyn FileHost>,
}

impl HostApplyPatchHandler {
    pub(crate) const fn new(workspace: PathBuf, files: std::sync::Arc<dyn FileHost>) -> Self {
        Self { workspace, files }
    }

    async fn apply(&self, patch: &str, lenient: bool) -> Result<String, String> {
        let hunks = parse(patch)?;
        validate_unique_targets(&hunks, |path| {
            normalize_path(&resolve(&self.workspace, path))
        })?;
        let mut initial_files = HashMap::new();
        for path in required_files_from_hunks(&hunks) {
            let source = resolve(&self.workspace, &path);
            let contents = self
                .files
                .read(&source)
                .await
                .and_then(|contents| contents.ok_or_else(|| "No such file or directory".into()))
                .map_err(|error| {
                    format!(
                        "Failed to read file to update {}: {error}",
                        source.display()
                    )
                })?;
            initial_files.insert(path, contents);
        }
        let plan = plan_hunks(hunks, &initial_files, lenient, |path| {
            normalize_path(&resolve(&self.workspace, path))
        })?;
        for operation in plan.operations() {
            match operation {
                PatchOperation::Write { path, contents } => {
                    let target = resolve(&self.workspace, path);
                    self.files.write(&target, contents).await.map_err(|error| {
                        format!("Failed to write file {}: {error}", target.display())
                    })?;
                }
                PatchOperation::Delete { path } => {
                    let target = resolve(&self.workspace, path);
                    self.files.remove(&target).await.map_err(|error| {
                        format!("Failed to delete file {}: {error}", target.display())
                    })?;
                }
            }
        }
        Ok(plan.summary().to_owned())
    }
}

impl Tool for HostApplyPatchHandler {
    fn definition(&self) -> ToolDefinition {
        StandardTool::ApplyPatch.definition()
    }

    fn execute<'life0, 'life1, 'async_trait>(
        &'life0 self,
        input: ToolInput,
        context: ToolContext<'life1>,
    ) -> nanocode_tools::ToolCallFuture<'async_trait, ToolResult>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let output: ToolResult = {
                let input = patch_input(input)?;
                Ok(match self.apply(&input, lenient(context)).await {
                    Ok(output) => ToolOutput::text(output).with_structured_result(json!({})),
                    Err(error) => {
                        ToolOutput::error(format!("apply_patch verification failed: {error}"))
                    }
                })
            };
            output
        })
    }
}

pub(crate) enum Patches {
    Host(std::sync::Arc<dyn FileHost>),
    #[cfg(not(target_family = "wasm"))]
    Local,
}

impl Patches {
    pub(crate) async fn apply(
        &self,
        patch: String,
        directory: PathBuf,
        context: ToolContext<'_>,
    ) -> Result<String, String> {
        let lenient = lenient(context);
        match self {
            Self::Host(files) => {
                HostApplyPatchHandler::new(directory, std::sync::Arc::clone(files))
                    .apply(&patch, lenient)
                    .await
                    .map_err(|error| format!("apply_patch verification failed: {error}"))
            }
            #[cfg(not(target_family = "wasm"))]
            Self::Local => {
                match nanocode_platform::spawn_blocking(move || {
                    apply_in(&patch, &directory, lenient)
                })
                .await
                {
                    Ok(Ok(output)) => Ok(output),
                    Ok(Err(error)) => Err(format!("apply_patch verification failed: {error}")),
                    Err(error) => Err(format!("apply_patch task failed: {error}")),
                }
            }
        }
    }
}

#[derive(serde::Deserialize)]
struct FunctionPatch {
    #[serde(alias = "patch")]
    input: String,
}

fn patch_input(input: ToolInput) -> Result<String, nanocode_tools::ToolInputError> {
    match input {
        ToolInput::Freeform(input) => Ok(input),
        ToolInput::Function(_) => input
            .decode_json::<FunctionPatch>()
            .map(|arguments| arguments.input),
    }
}

pub(crate) fn heredoc(script: &str) -> Result<Option<(String, Option<String>, String)>, String> {
    let Some((patch, directory, rest, quoted)) = parse_heredoc(script) else {
        return Ok(None);
    };
    if !quoted && patch.contains(['$', '\x60', '\\']) {
        return Err("apply_patch heredoc may require shell expansion; quote the delimiter or use the apply_patch tool with the intended literal patch".to_owned());
    }
    Ok(Some((patch, directory, rest)))
}

fn parse_heredoc(script: &str) -> Option<(String, Option<String>, String, bool)> {
    let script = script.trim_start();
    let (directory, invocation) = match cd_prefix(script) {
        Some((directory, rest)) => (Some(directory), rest),
        None => (None, script),
    };
    let redirect = ["apply_patch", "applypatch"]
        .iter()
        .find_map(|name| invocation.strip_prefix(name))?
        .trim_start_matches([' ', '\t'])
        .strip_prefix("<<")?
        .trim_start_matches([' ', '\t']);
    let (header, body) = redirect.split_once('\n')?;
    let delimiter = heredoc_delimiter(header.trim_end_matches([' ', '\t']))?;
    let quoted = header.starts_with(['\'', '"']);
    let mut lines = body.split('\n');
    let mut patch = Vec::new();
    loop {
        let line = lines.next()?;
        if line == delimiter {
            break;
        }
        patch.push(line);
    }
    Some((
        patch.join("\n").trim_end_matches('\n').to_owned(),
        directory,
        lines.collect::<Vec<_>>().join("\n").trim().to_owned(),
        quoted,
    ))
}

fn cd_prefix(script: &str) -> Option<(String, &str)> {
    let argument = script.strip_prefix("cd")?;
    let argument = argument
        .strip_prefix([' ', '\t'])?
        .trim_start_matches([' ', '\t']);
    let (directory, rest) = match argument.chars().next()? {
        '\'' => {
            let (directory, rest) = argument[1..].split_once('\'')?;
            (directory, rest)
        }
        '"' => {
            let (directory, rest) = argument[1..].split_once('"')?;
            (!directory.contains(['$', '\x60', '\\'])).then_some((directory, rest))?
        }
        _ => {
            let end = argument
                .find(|character: char| {
                    character.is_whitespace() || "'\"&|;<>()$\x60\\".contains(character)
                })
                .unwrap_or(argument.len());
            argument.split_at(end)
        }
    };
    let rest = rest
        .trim_start_matches([' ', '\t'])
        .strip_prefix("&&")?
        .trim_start_matches([' ', '\t']);
    (!directory.is_empty()).then(|| (directory.to_owned(), rest))
}

fn heredoc_delimiter(header: &str) -> Option<&str> {
    let delimiter = match header.chars().next()? {
        '\'' => header.strip_prefix('\'')?.strip_suffix('\'')?,
        '"' => header.strip_prefix('"')?.strip_suffix('"')?,
        _ => header,
    };
    (!delimiter.is_empty()
        && !delimiter.contains(|character: char| {
            character.is_whitespace() || "'\"&|;<>()".contains(character)
        }))
    .then_some(delimiter)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PatchOperation {
    Write { path: PathBuf, contents: String },

    Delete { path: PathBuf },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PatchPlan {
    operations: Vec<PatchOperation>,
    summary: String,
}

impl PatchPlan {
    #[must_use]
    pub fn operations(&self) -> &[PatchOperation] {
        &self.operations
    }

    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

pub fn required_files(patch: &str) -> Result<Vec<PathBuf>, String> {
    let hunks = parse(patch)?;
    Ok(required_files_from_hunks(&hunks))
}

fn required_files_from_hunks(hunks: &[Hunk]) -> Vec<PathBuf> {
    let mut produced = HashSet::new();
    let mut required = Vec::new();
    let mut retained = HashSet::new();
    for hunk in hunks {
        match hunk {
            Hunk::AddFile { path, .. } => {
                produced.insert(path.clone());
            }
            Hunk::DeleteFile { path } => {
                produced.remove(path);
            }
            Hunk::UpdateFile {
                path, move_path, ..
            } => {
                if !produced.contains(path) && retained.insert(path.clone()) {
                    required.push(path.clone());
                }
                produced.remove(path);
                produced.insert(move_path.as_ref().unwrap_or(path).clone());
            }
        }
    }
    required
}

pub fn plan(patch: &str, initial_files: &HashMap<PathBuf, String>) -> Result<PatchPlan, String> {
    let hunks = parse(patch)?;
    validate_unique_targets(&hunks, normalize_path)?;
    plan_hunks(
        hunks,
        initial_files,
        LENIENT_MATCHING.load(Ordering::Relaxed),
        normalize_path,
    )
}

fn plan_hunks(
    hunks: Vec<Hunk>,
    initial_files: &HashMap<PathBuf, String>,
    lenient: bool,
    resolve_target: impl Fn(&Path) -> PathBuf,
) -> Result<PatchPlan, String> {
    let mut files = initial_files.clone();
    let mut operations = Vec::new();
    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut deleted = Vec::<PathBuf>::new();

    for hunk in hunks {
        match hunk {
            Hunk::AddFile { path, contents } => {
                files.insert(path.clone(), contents.clone());
                if let Some(index) = deleted
                    .iter()
                    .position(|deleted| resolve_target(deleted) == resolve_target(&path))
                {
                    deleted.remove(index);
                    operations.retain(|operation| {
                        !matches!(operation, PatchOperation::Delete { path: deleted } if resolve_target(deleted) == resolve_target(&path))
                    });
                    modified.push(path.clone());
                } else {
                    added.push(path.clone());
                }
                operations.push(PatchOperation::Write { path, contents });
            }
            Hunk::DeleteFile { path } => {
                files.remove(&path);
                operations.push(PatchOperation::Delete { path: path.clone() });
                deleted.push(path);
            }
            Hunk::UpdateFile {
                path,
                move_path,
                chunks,
            } => {
                let original = files
                    .get(&path)
                    .ok_or_else(|| format!("Failed to read file to update {}", path.display()))?;
                let updated = apply_chunks(original, &chunks, &path, lenient)?;
                if let Some(destination) = move_path {
                    files.remove(&path);
                    files.insert(destination.clone(), updated.clone());
                    operations.push(PatchOperation::Write {
                        path: destination.clone(),
                        contents: updated,
                    });
                    operations.push(PatchOperation::Delete { path });
                    modified.push(destination);
                } else {
                    files.insert(path.clone(), updated.clone());
                    operations.push(PatchOperation::Write {
                        path: path.clone(),
                        contents: updated,
                    });
                    modified.push(path);
                }
            }
        }
    }

    let mut summary = String::from("Success. Updated the following files:\n");
    for path in &added {
        push_summary_line(&mut summary, 'A', path);
    }
    for path in &modified {
        push_summary_line(&mut summary, 'M', path);
    }
    for path in &deleted {
        push_summary_line(&mut summary, 'D', path);
    }
    Ok(PatchPlan {
        operations,
        summary,
    })
}

#[derive(Debug, PartialEq)]
struct ApplyPatchArgs {
    patch: String,
    hunks: Vec<Hunk>,
    workdir: Option<String>,
    environment_id: Option<String>,
}

#[cfg(not(target_family = "wasm"))]
pub fn apply(patch: &str, workspace: &Path) -> Result<String, String> {
    apply_in(patch, workspace, LENIENT_MATCHING.load(Ordering::Relaxed))
}

#[cfg(not(target_family = "wasm"))]
fn apply_in(patch: &str, workspace: &Path, lenient: bool) -> Result<String, String> {
    let hunks = parse(patch)?;
    validate_unique_targets(&hunks, |path| normalize_path(&resolve(workspace, path)))?;

    let mut initial_files = HashMap::new();
    for path in required_files_from_hunks(&hunks) {
        let source = resolve(workspace, &path);
        let contents = std::fs::read_to_string(&source).map_err(|error| {
            format!(
                "Failed to read file to update {}: {error}",
                source.display()
            )
        })?;
        initial_files.insert(path, contents);
    }
    let plan = plan_hunks(hunks, &initial_files, lenient, |path| {
        normalize_path(&resolve(workspace, path))
    })?;
    for operation in plan.operations() {
        match operation {
            PatchOperation::Write { path, contents } => {
                write_file(&resolve(workspace, path), contents.as_bytes())?;
            }
            PatchOperation::Delete { path } => {
                let target = resolve(workspace, path);
                std::fs::remove_file(&target).map_err(|error| {
                    format!("Failed to delete file {}: {error}", target.display())
                })?;
            }
        }
    }
    Ok(plan.summary().to_owned())
}

pub(crate) fn touched_paths(input: &serde_json::Value) -> Vec<String> {
    let patch = match input {
        serde_json::Value::String(patch) => patch.as_str(),
        serde_json::Value::Object(object) => match object
            .get("input")
            .or_else(|| object.get("patch"))
            .and_then(serde_json::Value::as_str)
        {
            Some(patch) => patch,
            None => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    let Ok(hunks) = parse(patch) else {
        return Vec::new();
    };
    hunks
        .into_iter()
        .flat_map(|hunk| match hunk {
            Hunk::AddFile { path, .. } | Hunk::DeleteFile { path } => vec![path],
            Hunk::UpdateFile {
                path, move_path, ..
            } => std::iter::once(path).chain(move_path).collect(),
        })
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

fn parse(patch: &str) -> Result<Vec<Hunk>, String> {
    let ApplyPatchArgs {
        hunks,
        patch: _,
        workdir: _,
        environment_id: _,
    } = parse_patch(patch).map_err(|error| error.to_string())?;
    if hunks.is_empty() {
        return Err("No files were modified.".to_owned());
    }
    Ok(hunks.into_iter().map(normalize_hunk_paths).collect())
}

fn validate_unique_targets(
    hunks: &[Hunk],
    mut resolve: impl FnMut(&Path) -> PathBuf,
) -> Result<(), String> {
    let mut targets = HashSet::new();
    let mut deleted = HashSet::new();
    for hunk in hunks {
        let (source, destination) = match hunk {
            Hunk::AddFile { path, .. } | Hunk::DeleteFile { path } => (path, None),
            Hunk::UpdateFile {
                path, move_path, ..
            } => (path, move_path.as_deref()),
        };

        if let Hunk::AddFile { .. } = hunk
            && deleted.remove(&resolve(source))
        {
            continue;
        }
        for path in std::iter::once(source.as_path()).chain(destination) {
            let target = resolve(path);
            if matches!(hunk, Hunk::DeleteFile { .. }) {
                deleted.insert(target.clone());
            }
            if !targets.insert(target.clone()) {
                return Err(format!("multiple operations target {}", target.display()));
            }
        }
    }
    Ok(())
}

fn normalize_hunk_paths(hunk: Hunk) -> Hunk {
    match hunk {
        Hunk::AddFile { path, contents } => Hunk::AddFile {
            path: normalize_path(&path),
            contents,
        },
        Hunk::DeleteFile { path } => Hunk::DeleteFile {
            path: normalize_path(&path),
        },
        Hunk::UpdateFile {
            path,
            move_path,
            chunks,
        } => Hunk::UpdateFile {
            path: normalize_path(&path),
            move_path: move_path.map(|path| normalize_path(&path)),
            chunks,
        },
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir
                if matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) =>
            {
                normalized.pop();
            }
            Component::ParentDir if normalized.has_root() => {}
            Component::ParentDir => normalized.push(component.as_os_str()),
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

fn apply_chunks(
    original: &str,
    chunks: &[UpdateFileChunk],
    path: &Path,
    lenient: bool,
) -> Result<String, String> {
    let trailing_newline = original.ends_with('\n');
    let mut original_lines = original
        .split('\n')
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if original_lines.last().is_some_and(String::is_empty) {
        original_lines.pop();
    }

    let mut lenient_error = None;
    let mut replacements = Vec::new();
    let mut line_index = 0;
    for chunk in chunks {
        if let Some(context) = &chunk.change_context {
            let context = [context.clone()];
            let error = || {
                format!(
                    "Failed to find context '{}' in {}",
                    context[0],
                    path.display()
                )
            };
            let found =
                match seek_sequence::seek_sequence(&original_lines, &context, line_index, false) {
                    Some(found) => found,
                    None if lenient => {
                        let found = seek_lenient(&original_lines, &context, line_index, false)
                            .ok_or_else(error)?;
                        lenient_error.get_or_insert_with(error);
                        found
                    }
                    None => return Err(error()),
                };
            line_index = found + 1;
        }

        if chunk.old_lines.is_empty() {
            replacements.push((original_lines.len(), 0, chunk.new_lines.clone()));
            continue;
        }

        let error = || {
            format!(
                "Failed to find expected lines in {}:\n{}",
                path.display(),
                chunk.old_lines.join("\n")
            )
        };
        let (found, old_lines, new_lines) = match locate(
            &original_lines,
            chunk,
            line_index,
            seek_sequence::seek_sequence,
        ) {
            Some(located) => located,
            None if lenient => {
                let located =
                    locate(&original_lines, chunk, line_index, seek_lenient).ok_or_else(error)?;
                lenient_error.get_or_insert_with(error);
                located
            }
            None => return Err(error()),
        };
        replacements.push((found, old_lines.len(), new_lines.to_vec()));
        line_index = found + old_lines.len();
    }

    replacements.sort_by_key(|(start, _, _)| *start);
    if let Some(error) = lenient_error
        && replacements
            .windows(2)
            .any(|pair| pair[0].0 + pair[0].1 > pair[1].0)
    {
        return Err(error);
    }
    for (start, old_len, new_lines) in replacements.into_iter().rev() {
        original_lines.splice(start..start + old_len, new_lines);
    }
    if trailing_newline {
        original_lines.push(String::new());
    }
    Ok(original_lines.join("\n"))
}

type Seek = fn(&[String], &[String], usize, bool) -> Option<usize>;

fn locate<'a>(
    lines: &[String],
    chunk: &'a UpdateFileChunk,
    start: usize,
    seek: Seek,
) -> Option<(usize, &'a [String], &'a [String])> {
    let old_lines = chunk.old_lines.as_slice();
    let new_lines = chunk.new_lines.as_slice();
    if let Some(found) = seek(lines, old_lines, start, chunk.is_end_of_file) {
        return Some((found, old_lines, new_lines));
    }
    if !old_lines.last().is_some_and(String::is_empty) {
        return None;
    }
    let old_lines = &old_lines[..old_lines.len() - 1];
    let new_lines = match new_lines.split_last() {
        Some((last, rest)) if last.is_empty() => rest,
        _ => new_lines,
    };
    seek(lines, old_lines, start, chunk.is_end_of_file).map(|found| (found, old_lines, new_lines))
}

fn seek_lenient(lines: &[String], pattern: &[String], start: usize, eof: bool) -> Option<usize> {
    seek_sequence::seek_sequence(lines, pattern, 0, eof)
        .or_else(|| seek_sequence::seek_whitespace(lines, pattern, start, eof))
        .or_else(|| seek_sequence::seek_whitespace(lines, pattern, 0, eof))
}

fn resolve(workspace: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        workspace.join(path)
    }
}

#[cfg(not(target_family = "wasm"))]
fn write_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Failed to create parent directories for {}: {error}",
                path.display()
            )
        })?;
    }
    std::fs::write(path, contents)
        .map_err(|error| format!("Failed to write file {}: {error}", path.display()))
}

fn push_summary_line(summary: &mut String, operation: char, path: &Path) {
    summary.push(operation);
    summary.push(' ');
    summary.push_str(&path.to_string_lossy());
    summary.push('\n');
}
