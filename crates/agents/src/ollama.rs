//! Local models served by Ollama: list the installed models and describe them as an OpenCode
//! provider. Hangar never calls the models; the agent CLI does.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde_json::{Map, Value, json};

pub const DEFAULT_HOST: &str = "http://127.0.0.1:11434";
const DEFAULT_PORT: u16 = 11434;

#[derive(Debug, thiserror::Error)]
pub enum OllamaError {
    #[error("invalid Ollama host `{0}`")]
    InvalidHost(String),
    #[error("not supported: {0}")]
    Unsupported(&'static str),
    #[error("Ollama is not running at {0}")]
    NotRunning(String),
    #[error("Ollama at {0} did not answer in time")]
    Timeout(String),
    #[error("Ollama answered HTTP {0}")]
    Http(u16),
    #[error("unexpected answer from Ollama: {0}")]
    Malformed(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// An Ollama server address (plain HTTP only).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OllamaHost {
    pub host: String,
    pub port: u16,
}

impl OllamaHost {
    /// Accepts `http://host[:port][/]` or `host[:port]`; an empty string is the default host.
    pub fn parse(input: &str) -> Result<Self, OllamaError> {
        let trimmed = input.trim();
        let rest = if trimmed.is_empty() {
            DEFAULT_HOST.trim_start_matches("http://")
        } else if trimmed.starts_with("https://") {
            return Err(OllamaError::Unsupported("https Ollama hosts"));
        } else {
            trimmed.strip_prefix("http://").unwrap_or(trimmed)
        };
        let rest = rest.trim_end_matches('/');
        if rest.is_empty() || rest.contains('/') {
            return Err(OllamaError::InvalidHost(input.to_owned()));
        }
        let (host, port) = match rest.rsplit_once(':') {
            Some((host, port)) => {
                let port = port
                    .parse()
                    .map_err(|_| OllamaError::InvalidHost(input.to_owned()))?;
                (host, port)
            }
            _ => (rest, DEFAULT_PORT),
        };
        if host.is_empty() {
            return Err(OllamaError::InvalidHost(input.to_owned()));
        }
        Ok(Self {
            host: host.to_owned(),
            port,
        })
    }

    /// `http://host:port`.
    pub fn url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OllamaModel {
    /// The model tag, e.g. `qwen3-coder:30b`.
    pub name: String,
    /// e.g. `30.5B`, when Ollama reports it.
    pub parameter_size: Option<String>,
}

/// The models installed on `host` (`GET /api/tags`). Blocking: call it off the UI thread.
pub fn list_models(host: &OllamaHost, timeout: Duration) -> Result<Vec<OllamaModel>, OllamaError> {
    let body = get(host, "/api/tags", timeout)?;
    parse_tags(&body)
}

fn get(host: &OllamaHost, path: &str, timeout: Duration) -> Result<Vec<u8>, OllamaError> {
    let url = host.url();
    let addr = (host.host.as_str(), host.port)
        .to_socket_addrs()
        .map_err(|_| OllamaError::InvalidHost(url.clone()))?
        .next()
        .ok_or_else(|| OllamaError::InvalidHost(url.clone()))?;
    let io_error = |err: std::io::Error| match err.kind() {
        ErrorKind::ConnectionRefused => OllamaError::NotRunning(url.clone()),
        ErrorKind::TimedOut | ErrorKind::WouldBlock => OllamaError::Timeout(url.clone()),
        _ => OllamaError::Io(err),
    };
    let mut stream = TcpStream::connect_timeout(&addr, timeout).map_err(io_error)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    // HTTP/1.0 so the server answers with a plain body (no chunked encoding) and closes.
    let request = format!(
        "GET {path} HTTP/1.0\r\nHost: {}:{}\r\nAccept: application/json\r\nConnection: close\r\n\r\n",
        host.host, host.port
    );
    stream.write_all(request.as_bytes()).map_err(io_error)?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).map_err(io_error)?;
    split_response(&response)
}

fn split_response(response: &[u8]) -> Result<Vec<u8>, OllamaError> {
    let end = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| OllamaError::Malformed("no HTTP header".to_owned()))?;
    let head = String::from_utf8_lossy(&response[..end]);
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| OllamaError::Malformed("no HTTP status".to_owned()))?;
    if !(200..300).contains(&status) {
        return Err(OllamaError::Http(status));
    }
    Ok(response[end + 4..].to_vec())
}

fn parse_tags(body: &[u8]) -> Result<Vec<OllamaModel>, OllamaError> {
    let value: Value =
        serde_json::from_slice(body).map_err(|err| OllamaError::Malformed(err.to_string()))?;
    let models = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| OllamaError::Malformed("no `models` list".to_owned()))?;
    Ok(models
        .iter()
        .filter_map(|model| {
            let name = model.get("name").and_then(Value::as_str)?;
            Some(OllamaModel {
                name: name.to_owned(),
                parameter_size: model
                    .pointer("/details/parameter_size")
                    .and_then(Value::as_str)
                    .filter(|size| !size.is_empty())
                    .map(str::to_owned),
            })
        })
        .collect())
}

pub const OPENCODE_PROVIDER_ID: &str = "ollama";

/// OpenCode config keys (`provider`, and `model` when `default_model` is set) that add `models`
/// as an OpenAI-compatible `ollama` provider. Merge them into `OPENCODE_CONFIG_CONTENT`.
pub fn opencode_provider(
    host: &OllamaHost,
    models: &[OllamaModel],
    default_model: Option<&str>,
) -> Map<String, Value> {
    let entries: Map<String, Value> = models
        .iter()
        .map(|model| {
            let label = match &model.parameter_size {
                Some(size) => format!("{} ({size})", model.name),
                None => model.name.clone(),
            };
            (model.name.clone(), json!({ "name": label }))
        })
        .collect();
    let mut config = Map::new();
    config.insert(
        "provider".to_owned(),
        json!({
            OPENCODE_PROVIDER_ID: {
                "npm": "@ai-sdk/openai-compatible",
                "name": "Ollama (local)",
                "options": { "baseURL": format!("{}/v1", host.url()) },
                "models": entries,
            }
        }),
    );
    if let Some(model) = default_model.filter(|m| models.iter().any(|known| known.name == *m)) {
        config.insert(
            "model".to_owned(),
            Value::String(format!("{OPENCODE_PROVIDER_ID}/{model}")),
        );
    }
    config
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    fn host(s: &str) -> OllamaHost {
        OllamaHost::parse(s).unwrap()
    }

    #[test]
    fn parses_hosts() {
        assert_eq!(host("").url(), DEFAULT_HOST);
        assert_eq!(
            host("http://localhost:11434/").url(),
            "http://localhost:11434"
        );
        assert_eq!(host("10.0.0.2").url(), "http://10.0.0.2:11434");
        assert_eq!(host("box.local:8080").url(), "http://box.local:8080");
        assert!(matches!(
            OllamaHost::parse("https://x"),
            Err(OllamaError::Unsupported(_))
        ));
        assert!(OllamaHost::parse("http://x:port").is_err());
        assert!(OllamaHost::parse("http://x/api").is_err());
        assert!(OllamaHost::parse(":11434").is_err());
    }

    /// Serves one canned HTTP response on a local port.
    fn serve(response: &'static str) -> OllamaHost {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            stream.write_all(response.as_bytes()).unwrap();
        });
        host(&format!("127.0.0.1:{port}"))
    }

    const TIMEOUT: Duration = Duration::from_secs(2);

    #[test]
    fn lists_models() {
        let host = serve(
            "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"models\":[{\"name\":\"qwen3-coder:30b\",\"details\":{\"parameter_size\":\"30.5B\"}},\
             {\"name\":\"llama3.2:latest\",\"details\":{}}]}",
        );
        let models = list_models(&host, TIMEOUT).unwrap();
        assert_eq!(
            models,
            vec![
                OllamaModel {
                    name: "qwen3-coder:30b".into(),
                    parameter_size: Some("30.5B".into())
                },
                OllamaModel {
                    name: "llama3.2:latest".into(),
                    parameter_size: None
                },
            ]
        );
    }

    #[test]
    fn reports_http_errors() {
        let host = serve("HTTP/1.0 500 Internal Server Error\r\n\r\noops");
        assert!(matches!(
            list_models(&host, TIMEOUT),
            Err(OllamaError::Http(500))
        ));
    }

    #[test]
    fn reports_garbage() {
        let host = serve("HTTP/1.0 200 OK\r\n\r\nnot json");
        assert!(matches!(
            list_models(&host, TIMEOUT),
            Err(OllamaError::Malformed(_))
        ));
    }

    #[test]
    fn reports_not_running() {
        // Port 1 (tcpmux) is privileged and unused: a freed ephemeral port could be taken by
        // another test's listener.
        let host = host("127.0.0.1:1");
        assert!(matches!(
            list_models(&host, TIMEOUT),
            Err(OllamaError::NotRunning(_))
        ));
    }

    #[test]
    fn builds_opencode_provider() {
        let models = vec![
            OllamaModel {
                name: "qwen3-coder:30b".into(),
                parameter_size: Some("30.5B".into()),
            },
            OllamaModel {
                name: "llama3.2".into(),
                parameter_size: None,
            },
        ];
        let config = opencode_provider(&host("127.0.0.1"), &models, Some("qwen3-coder:30b"));
        assert_eq!(
            Value::Object(config),
            json!({
                "provider": { "ollama": {
                    "npm": "@ai-sdk/openai-compatible",
                    "name": "Ollama (local)",
                    "options": { "baseURL": "http://127.0.0.1:11434/v1" },
                    "models": {
                        "qwen3-coder:30b": { "name": "qwen3-coder:30b (30.5B)" },
                        "llama3.2": { "name": "llama3.2" },
                    },
                } },
                "model": "ollama/qwen3-coder:30b",
            })
        );
        let config = opencode_provider(&host("127.0.0.1"), &models, Some("gone"));
        assert!(!config.contains_key("model"));
    }
}
