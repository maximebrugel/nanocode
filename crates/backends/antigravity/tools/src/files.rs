use std::fs;
use std::io::Write;
use std::path::Path;

pub const MAX_LINES: usize = 800;
pub const MAX_BYTES: usize = 46_080;

fn absolute(path: &str) -> Result<&Path, String> {
    let candidate = Path::new(path);
    match candidate.is_absolute() {
        true => Ok(candidate),
        false => Err(format!(
            "declaring permissions: failed to parse path: {path} must be an absolute path: path is not absolute"
        )),
    }
}

pub fn read_file(
    path: &str,
    start: Option<i64>,
    end: Option<i64>,
    offset: Option<i64>,
) -> Result<String, String> {
    let file = absolute(path)?;
    if let Some(start) = start
        && start < 1
    {
        return Err(format!("invalid StartLine {start}: must be at least 1"));
    }
    if let Some(end) = end
        && end < 1
    {
        return Err(format!("invalid EndLine {end}: must be at least 1"));
    }
    if let (Some(start), Some(end)) = (start, end)
        && start > end
    {
        return Err(format!(
            "invalid line range: StartLine {start} is greater than EndLine {end}"
        ));
    }
    let metadata = fs::metadata(file).map_err(|_| "file does not exist".to_owned())?;
    if metadata.is_dir() {
        return Err("path is a directory, not a file".to_owned());
    }
    let bytes = fs::read(file).map_err(|error| error.to_string())?;
    if bytes.is_empty() {
        return Ok("File is empty".to_owned());
    }
    let text = String::from_utf8_lossy(&bytes);
    let trailing = text.ends_with('\n');
    let mut lines = text.split('\n').collect::<Vec<_>>();
    if trailing {
        lines.pop();
    }
    let total = lines.len();
    let first = usize::try_from(start.unwrap_or(1))
        .unwrap_or(1)
        .clamp(1, total);
    let requested = end.map_or(total, |end| usize::try_from(end).unwrap_or(total));
    let last = requested.min(total).min(first + MAX_LINES - 1).max(first);
    let mut rendered = String::new();
    for (index, line) in lines.iter().enumerate().take(last).skip(first - 1) {
        rendered.push_str(&format!("{}: {}\n", index + 1, line.trim_end_matches('\r')));
    }
    let skip = usize::try_from(offset.unwrap_or(0)).unwrap_or(0);
    if skip == 0 && rendered.len() <= MAX_BYTES {
        rendered.pop();
        let mut out = format!("Showing lines {first}-{last} of {total}\n{rendered}");
        if !trailing && last == total {
            out.push_str("\n(no newline at end of file)");
        }
        return Ok(out);
    }
    let size = rendered.len();
    let from = floor(&rendered, skip.min(size));
    let to = floor(&rendered, (from + MAX_BYTES).min(size));
    let mut out = format!(
        "Partial read (lines {first}-{last} of {total}). Showing bytes {from}-{to} of {size} in this line range"
    );
    if to < size {
        out.push_str(&format!(
            ", truncated at the {MAX_BYTES}-byte limit. To read the rest of this line range, call this tool again with the same StartLine and EndLine and ByteOffset={to}"
        ));
    }
    out.push_str(".\n");
    out.push_str(&rendered[from..to]);
    Ok(out)
}

const fn floor(text: &str, mut index: usize) -> usize {
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

pub fn write_file(
    path: &str,
    content: &str,
    overwrite: bool,
    append: bool,
) -> Result<String, String> {
    let file = absolute(path)?;
    if overwrite && append {
        return Err("Overwrite and Append cannot both be true".to_owned());
    }
    if file.exists() && !overwrite && !append {
        return Err(format!(
            "{path} already exists. To overwrite it, retry with Overwrite=true. To edit it, view its contents first and use a code edit tool. Otherwise, choose a different file name"
        ));
    }
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    match append {
        true => fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
            .and_then(|mut handle| handle.write_all(content.as_bytes())),
        false => fs::write(file, content),
    }
    .map_err(|error| error.to_string())?;
    Ok("Created file.".to_owned())
}

pub fn edit_file(path: &str, target: &str, replacement: &str) -> Result<String, String> {
    let file = absolute(path)?;
    let Ok(text) = fs::read_to_string(file) else {
        return Err(format!(
            "{path} does not exist; the file may have been moved, renamed, or deleted"
        ));
    };
    let count = match target.is_empty() {
        true => 0,
        false => text.matches(target).count(),
    };
    let Some(first) = text.find(target).filter(|_| count > 0) else {
        return Err("target content not found in the file; the Target must exactly match existing file text, including whitespace".to_owned());
    };
    let line = text[..first].matches('\n').count() + 1;
    fs::write(file, text.replace(target, replacement)).map_err(|error| error.to_string())?;
    let noun = match count {
        1 => "replacement",
        _ => "replacements",
    };
    Ok(format!(
        "Made {count} {noun} in {path} starting at line {line} (+{}/-{} lines).",
        replacement.lines().count(),
        target.lines().count()
    ))
}
