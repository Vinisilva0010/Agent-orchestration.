//! Tools an agent may call.
//!
//! Each tool declares its arguments as a Rust type. The JSON schema sent to
//! the model is generated from that type, and the model's input is parsed into
//! it before anything runs, so the compiler and serde enforce the contract.
//! Argument types should use `#[serde(deny_unknown_fields)]`.

use std::{future::Future, marker::PhantomData, pin::Pin, sync::Arc};

use protocol::ToolSpec;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

pub type ToolFuture = Pin<Box<dyn Future<Output = Result<String, String>> + Send>>;

const MAX_ERROR_CHARS: usize = 500;
const MAX_NAME_IN_ERROR: usize = 100;

pub(crate) fn truncate(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value;
    /// Irreversible tools run only after a person approves the exact call.
    fn irreversible(&self) -> bool;
    /// Checks the input without running anything.
    fn validate(&self, input: &Value) -> Result<(), String>;
    fn run(&self, input: Value) -> ToolFuture;
}

fn parse<A: DeserializeOwned>(input: &Value) -> Result<A, String> {
    if !input.is_object() {
        return Err("tool input must be a JSON object".to_string());
    }
    serde_json::from_value(input.clone())
        .map_err(|error| truncate(&format!("invalid arguments: {error}"), MAX_ERROR_CHARS))
}

/// A tool with typed arguments `A`, implemented by an async function.
pub struct TypedTool<A, F> {
    name: String,
    description: String,
    irreversible: bool,
    run: F,
    _args: PhantomData<fn() -> A>,
}

impl<A, F, Fut> TypedTool<A, F>
where
    A: DeserializeOwned + JsonSchema,
    F: Fn(A) -> Fut,
    Fut: Future<Output = Result<String, String>> + Send + 'static,
{
    pub fn new(name: &str, description: &str, run: F) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            irreversible: false,
            run,
            _args: PhantomData,
        }
    }

    /// Marks the tool as irreversible: every call needs human approval.
    #[must_use]
    pub fn mark_irreversible(mut self) -> Self {
        self.irreversible = true;
        self
    }
}

impl<A, F, Fut> Tool for TypedTool<A, F>
where
    A: DeserializeOwned + JsonSchema + 'static,
    F: Fn(A) -> Fut + Send + Sync,
    Fut: Future<Output = Result<String, String>> + Send + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    /// The schema without `$schema` and `title`, which providers do not need.
    fn input_schema(&self) -> Value {
        let mut schema = serde_json::to_value(schemars::schema_for!(A))
            .unwrap_or_else(|_| json!({ "type": "object" }));
        if let Some(object) = schema.as_object_mut() {
            object.remove("$schema");
            object.remove("title");
        }
        schema
    }

    fn irreversible(&self) -> bool {
        self.irreversible
    }

    fn validate(&self, input: &Value) -> Result<(), String> {
        parse::<A>(input).map(|_| ())
    }

    fn run(&self, input: Value) -> ToolFuture {
        match parse::<A>(&input) {
            Ok(args) => Box::pin((self.run)(args)),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new(tools: Vec<Arc<dyn Tool>>) -> Result<Self, String> {
        for (index, tool) in tools.iter().enumerate() {
            if tools[..index]
                .iter()
                .any(|other| other.name() == tool.name())
            {
                return Err(format!("duplicate tool name: {}", tool.name()));
            }
        }
        Ok(Self { tools })
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools
            .iter()
            .map(|tool| ToolSpec {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                input_schema: tool.input_schema(),
            })
            .collect()
    }

    /// A registry with only the named tools, kept in this registry's order so
    /// the same set always produces the same tool definitions (and the same
    /// cacheable prefix) however the names are listed. Unknown names are an
    /// error. Pick the set once per run: changing it mid-run breaks caching.
    pub fn only(&self, names: &[&str]) -> Result<Self, String> {
        if let Some(unknown) = names
            .iter()
            .find(|name| !self.tools.iter().any(|tool| tool.name() == **name))
        {
            return Err(format!(
                "unknown tool: {}",
                truncate(unknown, MAX_NAME_IN_ERROR)
            ));
        }
        let tools = self
            .tools
            .iter()
            .filter(|tool| names.contains(&tool.name()))
            .cloned()
            .collect();
        Ok(Self { tools })
    }

    /// Returns the tool if it exists and the input is valid, or an error
    /// message for the model.
    pub fn validate(&self, name: &str, input: &Value) -> Result<&dyn Tool, String> {
        let tool = self
            .tools
            .iter()
            .find(|tool| tool.name() == name)
            .ok_or_else(|| format!("unknown tool: {}", truncate(name, MAX_NAME_IN_ERROR)))?;
        tool.validate(input)
            .map_err(|error| format!("{name}: {error}"))?;
        Ok(tool.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use schemars::JsonSchema;
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct Args {
        #[allow(dead_code)]
        report_id: String,
    }

    fn tool() -> Arc<dyn Tool> {
        Arc::new(TypedTool::new("archive", "Archives.", |_: Args| async {
            Ok("done".to_string())
        }))
    }

    #[test]
    fn schema_comes_from_the_type_without_meta_fields() {
        let schema = tool().input_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"][0], "report_id");
        assert_eq!(schema["additionalProperties"], false);
        assert!(schema.get("$schema").is_none());
        assert!(schema.get("title").is_none());
    }

    #[test]
    fn validation_rejects_bad_input() {
        let registry = ToolRegistry::new(vec![tool()]).unwrap();
        assert!(
            registry
                .validate("archive", &json!({ "report_id": "q3" }))
                .is_ok()
        );
        for bad in [
            json!({}),
            json!({ "report_id": "q3", "extra": 1 }),
            json!("not an object"),
            json!({ "_unparsed_arguments": "{" }),
        ] {
            assert!(registry.validate("archive", &bad).is_err(), "{bad}");
        }
        assert!(
            registry
                .validate("delete_everything", &json!({}))
                .err()
                .unwrap()
                .starts_with("unknown tool")
        );
    }

    #[test]
    fn rejects_duplicate_names() {
        assert!(ToolRegistry::new(vec![tool(), tool()]).is_err());
    }
}

#[cfg(test)]
mod subset_tests {
    use schemars::JsonSchema;
    use serde::Deserialize;
    use serde_json::json;

    use super::*;

    #[derive(Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct NoArgs {}

    fn tool(name: &str) -> Arc<dyn Tool> {
        Arc::new(TypedTool::new(name, "test tool", |_: NoArgs| async {
            Ok(String::from("ok"))
        }))
    }

    fn registry() -> ToolRegistry {
        ToolRegistry::new(vec![tool("read"), tool("write"), tool("delete")]).unwrap()
    }

    fn bytes(registry: &ToolRegistry) -> Vec<u8> {
        serde_json::to_vec(&registry.specs()).unwrap()
    }

    #[test]
    fn a_subset_keeps_registry_order_whatever_order_names_come_in() {
        let all = registry();
        let names: Vec<String> = all
            .only(&["delete", "read"])
            .unwrap()
            .specs()
            .into_iter()
            .map(|spec| spec.name)
            .collect();
        assert_eq!(names, ["read", "delete"]);
        assert_eq!(
            bytes(&all.only(&["read", "delete"]).unwrap()),
            bytes(&all.only(&["delete", "read", "read"]).unwrap())
        );
    }

    #[test]
    fn tools_outside_the_subset_cannot_be_called() {
        let reader = registry().only(&["read"]).unwrap();
        assert!(reader.validate("read", &json!({})).is_ok());
        let error = reader.validate("delete", &json!({})).err().unwrap();
        assert!(error.starts_with("unknown tool"), "{error}");
    }

    #[test]
    fn unknown_names_are_rejected() {
        assert!(registry().only(&["read", "drop_database"]).is_err());
    }
}
