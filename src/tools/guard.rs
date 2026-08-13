//! Context guardrails shared by the search tools.
//!
//! grep output lines look like `path:line: content`; this module drops
//! the lines whose path is sensitive (same `tools.sensitive_paths`
//! policy as read_file) so secrets cannot enter the context through
//! search results either.

use crate::config::ToolsConfig;
use crate::tools::scrub;

/// Split grep-style output lines into (kept, hidden_count): a line is
/// hidden when the path portion before the first `:` matches a
/// sensitive-path pattern (or the whole line matches, as a fallback for
/// unparsable lines).
pub fn filter_sensitive_lines(output: &str, config: &ToolsConfig) -> (String, usize) {
    let mut kept = Vec::new();
    let mut hidden = 0usize;
    for line in output.lines() {
        let path = line.split(':').next().unwrap_or(line);
        if scrub::is_sensitive_path(path, &config.sensitive_paths)
            || scrub::is_sensitive_path(line, &config.sensitive_paths)
        {
            hidden += 1;
            continue;
        }
        kept.push(line);
    }
    (kept.join("\n"), hidden)
}
