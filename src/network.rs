//! Fixed network destinations and the single direct-request construction site.

pub const ANTHROPIC_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const ANTHROPIC_PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
pub const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
pub const GITHUB_LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/Dvaderfun/claudometer/releases/latest";
// Owner/name also live in Cargo.toml `repository` — keep in sync.
pub const GITHUB_REPOSITORY_URL: &str = "https://github.com/Dvaderfun/claudometer";
pub const CLAUDE_CODE_GETTING_STARTED_URL: &str =
    "https://docs.anthropic.com/en/docs/claude-code/getting-started";

pub fn get(agent: &ureq::Agent, url: &str) -> ureq::Request {
    agent.get(url)
}
