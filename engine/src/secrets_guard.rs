//! The engine runs next to the database and the token issuer, but it must
//! never hold provider keys or the vault master key: only the gateway does.
//! Problems name variables and never print their values.

use thiserror::Error;

/// The vault master key, and the gateway admin secret that changes keys and
/// limits. The engine only needs the issuer secret.
const FORBIDDEN_NAMES: [&str; 2] = ["GATEWAY_MASTER_KEY", "GATEWAY_ADMIN_SECRET"];
const FORBIDDEN_NAME_PARTS: [&str; 2] = ["API_KEY", "PRIVATE_KEY"];
/// Value prefixes of provider keys (OpenAI and Anthropic, Google, xAI).
const PROVIDER_KEY_PREFIXES: [&str; 4] = ["sk-", "AIza", "AQ.", "xai-"];

#[derive(Debug, Error, PartialEq, Eq)]
#[error("unsafe environment: {}", .0.join("; "))]
pub struct UnsafeEnvironment(pub Vec<String>);

pub fn find_problems<I, K, V>(vars: I) -> Vec<String>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let mut problems: Vec<String> = vars
        .into_iter()
        .filter_map(|(name, value)| {
            let name = name.as_ref();
            let upper = name.to_ascii_uppercase();
            if FORBIDDEN_NAMES.contains(&upper.as_str())
                || FORBIDDEN_NAME_PARTS.iter().any(|part| upper.contains(part))
            {
                Some(format!("{name}: only the gateway may hold provider keys"))
            } else if PROVIDER_KEY_PREFIXES
                .iter()
                .any(|prefix| value.as_ref().trim().starts_with(prefix))
            {
                Some(format!("{name}: value looks like a provider key"))
            } else {
                None
            }
        })
        .collect();
    problems.sort();
    problems
}

/// Checks the current process environment. Variables that are not valid
/// UTF-8 are skipped: provider keys are always ASCII.
pub fn check_environment() -> Result<(), UnsafeEnvironment> {
    let vars = std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)));
    let problems = find_problems(vars);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(UnsafeEnvironment(problems))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_engine_settings() {
        let vars = [
            ("PATH", "/usr/bin"),
            ("DATABASE_URL", "postgres://u:p@127.0.0.1/db"),
            ("ENGINE_GATEWAY_URL", "http://127.0.0.1:8081"),
        ];
        assert!(find_problems(vars).is_empty());
    }

    #[test]
    fn rejects_provider_keys_by_name_and_value() {
        for name in [
            "GATEWAY_MASTER_KEY",
            "GEMINI_API_KEY",
            "openai_api_key",
            "SSH_PRIVATE_KEY",
        ] {
            assert_eq!(find_problems([(name, "x")]).len(), 1, "{name}");
        }
        for value in [
            "sk-proj-abc",
            "sk-ant-abc",
            "AIzaSyabc",
            "AQ.Ab8abc",
            "xai-abc",
        ] {
            let problems = find_problems([("HARMLESS", value)]);
            assert_eq!(problems.len(), 1, "{value}");
            assert!(!problems[0].contains(value), "values must never be printed");
        }
    }

    #[test]
    fn error_message_lists_names_only() {
        let error = UnsafeEnvironment(find_problems([("GEMINI_API_KEY", "AQ.secret-value")]));
        let text = error.to_string();
        assert!(text.contains("GEMINI_API_KEY"));
        assert!(!text.contains("secret-value"));
    }
}
