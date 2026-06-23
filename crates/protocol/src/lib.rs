use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct Request {
    pub id: u64,
    pub method: String,
    #[serde(default = "default_params")]
    pub params: Value,
}

impl Request {
    /// Parses one newline-delimited JSON-RPC request.
    ///
    /// # Errors
    ///
    /// Returns a serde error when the line is not valid JSON or does not match
    /// the request shape.
    pub fn from_json_line(line: &str) -> serde_json::Result<Self> {
        serde_json::from_str(line.trim_end())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Response {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

impl Response {
    #[must_use]
    pub fn success(id: u64, result: Value) -> Self {
        Self {
            id,
            result: Some(result),
            error: None,
        }
    }

    #[must_use]
    pub fn error(id: u64, message: impl Into<String>) -> Self {
        Self {
            id,
            result: None,
            error: Some(ResponseError {
                message: message.into(),
            }),
        }
    }

    #[must_use]
    pub fn to_json_line(&self) -> String {
        let mut line = serde_json::to_string(self).unwrap_or_else(|_| {
            "{\"id\":0,\"error\":{\"message\":\"response serialization failed\"}}".to_owned()
        });
        line.push('\n');
        line
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ResponseError {
    pub message: String,
}

fn default_params() -> Value {
    Value::Object(serde_json::Map::new())
}
