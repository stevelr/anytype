// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;

use futures::StreamExt as _;
use reqwest::{Client, Method, StatusCode, header::HeaderValue};
use serde_json::{Map, Value};

use super::{
    config::{HttpStep, Oracle},
    protocol::canonical_bytes,
    secret::SecretSet,
};

const HTTP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(180);
const MAX_HTTP_BODY: usize = 8 * 1024 * 1024;

pub struct RawHttpOracle {
    client: Client,
    base_url: reqwest::Url,
    authorization: HeaderValue,
}

impl RawHttpOracle {
    pub fn new(
        config: &Oracle,
        secrets: &SecretSet,
        credential_index: usize,
    ) -> Result<Self, String> {
        let base_url = reqwest::Url::parse(&config.base_url)
            .map_err(|_| "oracle base URL is invalid".to_owned())?;
        let secret = secrets.value(credential_index)?;
        let mut header = if secret.starts_with(b"Bearer ") {
            secret.to_vec()
        } else {
            let mut value = b"Bearer ".to_vec();
            value.extend_from_slice(secret);
            value
        };
        let authorization = HeaderValue::from_bytes(&header)
            .map_err(|_| "oracle credential is not a valid header value".to_owned())?;
        zeroize::Zeroize::zeroize(&mut header);
        let client = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(HTTP_DEADLINE)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "cannot build raw HTTP oracle".to_owned())?;
        Ok(Self {
            client,
            base_url,
            authorization,
        })
    }

    pub async fn execute(
        &self,
        step: &HttpStep,
        variables: &mut BTreeMap<String, Value>,
    ) -> Result<Value, String> {
        let path = render_text(&step.path, variables)?;
        let url = self
            .base_url
            .join(path.trim_start_matches('/'))
            .map_err(|_| "oracle path cannot be joined to its base URL".to_owned())?;
        if url.origin() != self.base_url.origin() {
            return Err("oracle path changed the configured origin".to_owned());
        }
        let method = Method::from_bytes(step.method.as_bytes())
            .map_err(|_| "oracle HTTP method is invalid".to_owned())?;
        let mut request = self
            .client
            .request(method, url)
            .header(reqwest::header::AUTHORIZATION, self.authorization.clone())
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(body) = &step.body {
            request = request.json(&render_value(body, variables)?);
        }
        let response = request
            .send()
            .await
            .map_err(|_| "raw HTTP oracle transport failed".to_owned())?;
        if response.status()
            != StatusCode::from_u16(step.expect_status)
                .map_err(|_| "raw HTTP oracle configured an invalid expected status".to_owned())?
        {
            return Err(format!(
                "raw HTTP oracle returned status category {} instead of the expected category",
                status_category(response.status())
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > u64::try_from(MAX_HTTP_BODY).unwrap_or(u64::MAX))
        {
            return Err("raw HTTP oracle response exceeds the body bound".to_owned());
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "cannot read raw HTTP oracle response".to_owned())?;
            let next = bytes
                .len()
                .checked_add(chunk.len())
                .ok_or_else(|| "raw HTTP oracle body length overflowed".to_owned())?;
            if next > MAX_HTTP_BODY {
                return Err("raw HTTP oracle response exceeds the body bound".to_owned());
            }
            bytes.extend_from_slice(&chunk);
        }
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .map_err(|_| "raw HTTP oracle response is not JSON".to_owned())?
        };
        for (name, pointer) in &step.capture {
            let captured = value
                .pointer(pointer)
                .cloned()
                .ok_or_else(|| "raw HTTP oracle capture pointer is absent".to_owned())?;
            variables.insert(name.clone(), captured);
        }
        Ok(value)
    }

    pub async fn fetch_openapi(&self, path: &str) -> Result<(String, Value), String> {
        let step = HttpStep {
            method: "GET".to_owned(),
            path: path.to_owned(),
            body: None,
            expect_status: 200,
            capture: BTreeMap::new(),
        };
        let value = self.execute(&step, &mut BTreeMap::new()).await?;
        let canonical = canonical_bytes(&value)?;
        Ok((sha256(&canonical), value))
    }
}

pub fn render_value(value: &Value, variables: &BTreeMap<String, Value>) -> Result<Value, String> {
    match value {
        Value::String(text) => {
            if let Some(name) = text
                .strip_prefix("${")
                .and_then(|tail| tail.strip_suffix('}'))
            {
                return variables
                    .get(name)
                    .cloned()
                    .ok_or_else(|| "template references an unknown variable".to_owned());
            }
            Ok(Value::String(render_text(text, variables)?))
        }
        Value::Array(values) => values
            .iter()
            .map(|value| render_value(value, variables))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone(), render_value(value, variables)?)))
            .collect::<Result<Map<_, _>, String>>()
            .map(Value::Object),
        scalar => Ok(scalar.clone()),
    }
}

fn render_text(text: &str, variables: &BTreeMap<String, Value>) -> Result<String, String> {
    let mut rendered = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        rendered.push_str(&rest[..start]);
        let tail = &rest[start + 2..];
        let end = tail
            .find('}')
            .ok_or_else(|| "template contains an unterminated variable".to_owned())?;
        let name = &tail[..end];
        let value = variables
            .get(name)
            .ok_or_else(|| "template references an unknown variable".to_owned())?;
        match value {
            Value::String(value) => rendered.push_str(value),
            Value::Number(value) => rendered.push_str(&value.to_string()),
            Value::Bool(value) => rendered.push_str(if *value { "true" } else { "false" }),
            _ => return Err("path template variable must be a JSON scalar".to_owned()),
        }
        rest = &tail[end + 1..];
    }
    rendered.push_str(rest);
    Ok(rendered)
}

fn status_category(status: StatusCode) -> &'static str {
    match status.as_u16() {
        100..=199 => "informational",
        200..=299 => "success",
        300..=399 => "redirect",
        400..=499 => "client-error",
        _ => "server-error",
    }
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(bytes);
    let mut result = String::with_capacity(64);
    for byte in digest {
        let _ = std::fmt::Write::write_fmt(&mut result, format_args!("{byte:02x}"));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_exact_values_without_stringifying_objects() {
        let variables = BTreeMap::from([
            ("id".to_owned(), json!("object-1")),
            ("body".to_owned(), json!({"name": "fixture"})),
        ]);
        let rendered = render_value(
            &json!({"id": "${id}", "object": "${body}", "path": "/v1/${id}"}),
            &variables,
        )
        .expect("render closed template");
        assert_eq!(
            rendered,
            json!({"id": "object-1", "object": {"name": "fixture"}, "path": "/v1/object-1"})
        );
    }
}
