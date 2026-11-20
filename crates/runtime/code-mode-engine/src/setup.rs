use djs_embed::{Builder, Data, HostFn, Loader, ModuleSource, Snapshot, Source};

const BOOTSTRAP: &str = include_str!("bootstrap.js");
const CODE_TOOLS: &str = include_str!("code-tools.mjs");
const CODE_VALUES: &str = include_str!("code-values.mjs");
const WEB: &str = include_str!("web.js");

const NODE_MODULES: [(&str, &str); 20] = [
    ("buffer", include_str!("node/buffer.mjs")),
    ("events", include_str!("node/events.mjs")),
    ("path", include_str!("node/path.mjs")),
    ("util", include_str!("node/util.mjs")),
    ("os", include_str!("node/os.mjs")),
    ("process", include_str!("node/process.mjs")),
    ("fs", include_str!("node/fs.mjs")),
    ("fs/promises", include_str!("node/fs-promises.mjs")),
    ("crypto", include_str!("node/crypto.mjs")),
    ("zlib", include_str!("node/zlib.mjs")),
    ("assert", include_str!("node/assert.mjs")),
    ("assert/strict", include_str!("node/assert-strict.mjs")),
    ("url", include_str!("node/url.mjs")),
    ("querystring", include_str!("node/querystring.mjs")),
    ("string_decoder", include_str!("node/string-decoder.mjs")),
    ("timers", include_str!("node/timers.mjs")),
    ("timers/promises", include_str!("node/timers-promises.mjs")),
    ("child_process", include_str!("node/child-process.mjs")),
    ("module", include_str!("node/module.mjs")),
    ("internal/workspace", include_str!("node/host.mjs")),
];

const NODE_GLOBALS: &str = include_str!("node/globals.mjs");
const NODE_GLOBALS_MODULE: &str = "nanocode:node";

struct NodeModules;

impl Loader for NodeModules {
    fn resolve(&self, specifier: &str, referrer: Option<&str>) -> Result<String, String> {
        let name = specifier.strip_prefix("node:").unwrap_or(specifier);
        let internal = name.starts_with("internal/");
        let builtin = referrer.is_some_and(|referrer| referrer.starts_with("node:"));
        if NODE_MODULES.iter().any(|(known, _)| *known == name) && (!internal || builtin) {
            Ok(format!("node:{name}"))
        } else if specifier.starts_with("node:") {
            Err(format!("No such built-in module: {specifier}"))
        } else {
            Err(format!("Cannot find module '{specifier}'"))
        }
    }

    fn load(&self, key: &str) -> Result<ModuleSource, String> {
        let name = key.strip_prefix("node:").unwrap_or(key);
        NODE_MODULES
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, text)| ModuleSource::js(*text))
            .ok_or_else(|| format!("No such built-in module: {key}"))
    }
}

pub(crate) fn node(builder: Builder) -> Builder {
    let mut builder = builder.loader(NodeModules);
    for (name, text) in NODE_MODULES {
        builder = builder.source(Source::module(format!("node:{name}"), text));
    }
    builder.source(Source::module(NODE_GLOBALS_MODULE, NODE_GLOBALS))
}

pub(crate) const BOOTSTRAP_MODULE: &str = "nanocode:bootstrap";
pub(crate) const PREPARE_CELL: &str = "prepareCell";
pub(crate) const RUN_CELL: &str = "runCell";

pub(crate) const TOOL: u32 = 1;
pub(crate) const CONTENT: u32 = 2;
pub(crate) const TEXT: u32 = 3;
pub(crate) const LOAD: u32 = 4;
pub(crate) const NOTIFY: u32 = 5;
pub(crate) const YIELD: u32 = 6;
pub(crate) const SET_TIMEOUT: u32 = 7;
pub(crate) const CLEAR_TIMEOUT: u32 = 8;
pub(crate) const VALUE: u32 = 9;
pub(crate) const SETTLED: u32 = 10;
pub(crate) const NEXT: u32 = 11;
pub(crate) const CATALOG: u32 = 12;
pub(crate) const QUIET: u32 = 13;
pub(crate) const EXIT: u32 = 14;
pub(crate) const STORE: u32 = 15;
pub(crate) const SETTLE: u32 = 16;
pub(crate) const SYNC_TOOL: u32 = 17;

const HOST_FNS: [(u32, &str); 15] = [
    (TOOL, "__nanocodeTool"),
    (SYNC_TOOL, "__nanocodeSyncTool"),
    (CONTENT, "__nanocodeContent"),
    (TEXT, "__nanocodeText"),
    (LOAD, "__nanocodeLoad"),
    (STORE, "__nanocodeStore"),
    (NOTIFY, "__nanocodeNotify"),
    (YIELD, "__nanocodeYield"),
    (SET_TIMEOUT, "__nanocodeSetTimeout"),
    (CLEAR_TIMEOUT, "__nanocodeClearTimeout"),
    (VALUE, "__nanocodeValue"),
    (SETTLED, "__nanocodeSettled"),
    (CATALOG, "__nanocodeCatalog"),
    (EXIT, "__nanocodeExit"),
    (SETTLE, "__nanocodeSettle"),
];

pub(crate) const BYTES_PUT: u32 = 20;
pub(crate) const BYTES_TAKE: u32 = 21;
pub(crate) const BYTES_TEXT: u32 = 22;
pub(crate) const BYTES_DISCARD: u32 = 23;
pub(crate) const BYTES_DECODE: u32 = 24;
pub(crate) const BYTES_ENCODE: u32 = 25;
pub(crate) const BYTES_TO_BASE64: u32 = 26;
pub(crate) const BYTES_FROM_BASE64: u32 = 27;
pub(crate) const BYTES_DIGEST: u32 = 28;
pub(crate) const BYTES_RANDOM: u32 = 29;

// The byte helpers back the prelude's buffers and the web globals (crypto and
// digests), so every snapshot registers them.
const BYTES_HOST_FNS: [(u32, &str); 10] = [
    (BYTES_PUT, "__nanocodeBytesPut"),
    (BYTES_TAKE, "__nanocodeBytesTake"),
    (BYTES_TEXT, "__nanocodeBytesText"),
    (BYTES_DISCARD, "__nanocodeBytesDiscard"),
    (BYTES_DECODE, "__nanocodeBytesDecode"),
    (BYTES_ENCODE, "__nanocodeBytesEncode"),
    (BYTES_TO_BASE64, "__nanocodeBytesToBase64"),
    (BYTES_FROM_BASE64, "__nanocodeBytesFromBase64"),
    (BYTES_DIGEST, "__nanocodeBytesDigest"),
    (BYTES_RANDOM, "__nanocodeBytesRandom"),
];

pub(crate) enum Definitions<'a> {
    AtSetup(&'a str),
    PerCell,
}

pub(crate) fn builder(definitions: &Definitions<'_>, prelude_at_setup: bool) -> Builder {
    let builder = Snapshot::builder().frozen_builtins(false);
    let builder = match definitions {
        Definitions::AtSetup(definitions) => {
            builder.global("__nanocodeDefinitions", Data::from(*definitions))
        }
        Definitions::PerCell => builder,
    };
    let mut builder = builder
        .global("__nanocodePreludeAtSetup", Data::Bool(prelude_at_setup))
        .host_fn(HostFn::asynchronous(NEXT, "__nanocodeNext"))
        .setup_probe(HostFn::sync(QUIET, "__nanocodeQuiet"));
    for (id, name) in HOST_FNS.into_iter().chain(BYTES_HOST_FNS) {
        builder = builder.host_fn(HostFn::sync(id, name));
    }
    builder
}

pub(crate) fn bootstrap(definitions: &Definitions<'_>) -> Result<Source, String> {
    let values = CODE_VALUES
        .split_once("export const")
        .ok_or_else(|| "shared value helper export marker is missing".to_owned())?
        .0;
    let prepare = format!(
        "{}(createCodeTools, createValueHelpers(), {})",
        BOOTSTRAP.trim_end(),
        WEB.trim_end()
    );
    let exports = match definitions {
        Definitions::AtSetup(_) => format!("export const {RUN_CELL} = {prepare};\n"),
        Definitions::PerCell => format!(
            "const prepareCellWith = {prepare};\nexport let {RUN_CELL};\nexport const {PREPARE_CELL} = (definitions) => {{ {RUN_CELL} = prepareCellWith(definitions); }};\n"
        ),
    };
    Ok(Source::module(
        BOOTSTRAP_MODULE,
        format!("{CODE_TOOLS}\n{values}\n{exports}"),
    ))
}

pub(crate) fn per_cell() -> Result<Builder, String> {
    Ok(node(
        builder(&Definitions::PerCell, false).source(bootstrap(&Definitions::PerCell)?),
    ))
}
