//! 交给云端识别（OpenAI 兼容的 `/audio/transcriptions`、豆包的录音文件识别极速版），和用大模型
//! 润色识别出的文字（OpenAI 兼容的 `/chat/completions`）。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use serde_json::{Value, json};

use crate::{
    SAMPLE_RATE,
    http::{self, Body, Response},
};

const DOUBAO_URL: &str = "https://openspeech.bytedance.com/api/v3/auc/bigmodel/recognize/flash";
const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cloud {
    /// `url` 是 API 的根（如 `https://api.openai.com/v1`），后面接 `/audio/transcriptions`。
    OpenAi { url: String, key: String, model: String },
    /// `app_id` 为空时 `key` 是新版控制台的 API Key，否则是旧版控制台的 Access Token。
    Doubao { app_id: String, key: String },
}

/// 润色用的大模型；`url` 是 API 的根，后面接 `/chat/completions`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Polish {
    pub url: String,
    pub key: String,
    pub model: String,
}

/// 把 `SAMPLE_RATE` 的单声道录音交给云端识别。`words` 是热词，帮云端认出专有名词。
pub fn transcribe_cloud(cloud: &Cloud, samples: &[f32], words: &[String]) -> anyhow::Result<String> {
    let audio = http::wav(samples, SAMPLE_RATE);
    match cloud {
        Cloud::OpenAi { url, key, model } => {
            let mut fields = vec![("model", model.clone()), ("response_format", "json".to_owned())];
            if !words.is_empty() {
                fields.push(("prompt", words.join(", ")));
            }
            let response = http::post(
                &endpoint(url, "audio/transcriptions"),
                &[format!("Authorization: Bearer {key}")],
                &[],
                Body::Form { fields, file: ("file", audio) },
                TIMEOUT,
            )?;
            Ok(field(&json_ok(&response)?, &["text"])?.trim().to_owned())
        }
        Cloud::Doubao { app_id, key } => {
            let secret = if app_id.is_empty() {
                vec![format!("X-Api-Key: {key}")]
            } else {
                vec![format!("X-Api-App-Key: {app_id}"), format!("X-Api-Access-Key: {key}")]
            };
            let headers = [
                "X-Api-Resource-Id: volc.bigasr.auc_turbo".to_owned(),
                format!("X-Api-Request-Id: {}", request_id()),
                "X-Api-Sequence: -1".to_owned(),
            ];
            let response = http::post(DOUBAO_URL, &secret, &headers, Body::Json(&doubao_body(&audio, words)), TIMEOUT)?;
            doubao_text(&response)
        }
    }
}

/// 让大模型改正识别出的文字里的同音错字、标点和大小写。出错时由调用方决定用不用原文。
pub fn polish(polish: &Polish, text: &str, words: &[String]) -> anyhow::Result<String> {
    let body = json!({
        "model": polish.model,
        "messages": [
            {"role": "system", "content": polish_prompt(words)},
            {"role": "user", "content": text},
        ],
    });
    let response = http::post(
        &endpoint(&polish.url, "chat/completions"),
        &[format!("Authorization: Bearer {}", polish.key)],
        &[],
        Body::Json(&body),
        TIMEOUT,
    )?;
    let reply = field(&json_ok(&response)?, &["choices", "0", "message", "content"])?.trim().to_owned();
    if reply.is_empty() { Ok(text.to_owned()) } else { Ok(reply) }
}

fn polish_prompt(words: &[String]) -> String {
    let mut prompt = String::from(
        "You clean up speech recognition output that will be typed into a terminal or sent to a \
         coding agent. Fix misrecognized words (especially homophones and technical terms), \
         punctuation and casing. Remove filler words and false starts. Keep the speaker's language, \
         meaning and wording otherwise; do not answer, translate or add anything. Reply with the \
         corrected text only.",
    );
    if !words.is_empty() {
        prompt.push_str("\nTerms the speaker may use: ");
        prompt.push_str(&words.join(", "));
    }
    prompt
}

fn doubao_body(audio: &[u8], words: &[String]) -> Value {
    let mut request = json!({"model_name": "bigmodel", "enable_itn": true, "enable_punc": true});
    if !words.is_empty() {
        let hotwords: Vec<Value> = words.iter().map(|word| json!({"word": word})).collect();
        request["corpus"] = json!({"context": json!({"hotwords": hotwords}).to_string()});
    }
    json!({
        "user": {"uid": "runode"},
        "audio": {
            "data": http::base64(audio),
            "format": "wav",
            "rate": SAMPLE_RATE,
            "bits": 16,
            "channel": 1,
        },
        "request": request,
    })
}

/// 豆包在 `X-Api-Status-Code` 里报结果：`20000000` 成功，`20000003` 是没听到说话。
fn doubao_text(response: &Response) -> anyhow::Result<String> {
    match response.api_status.as_str() {
        "20000000" => Ok(field(&serde_json::from_str(&response.body)?, &["result", "text"])?.trim().to_owned()),
        "20000003" => Ok(String::new()),
        "" => bail!("HTTP {}: {}", response.status, response.body.trim()),
        code => bail!("doubao {code}: {}", response.body.trim()),
    }
}

fn endpoint(root: &str, path: &str) -> String {
    format!("{}/{path}", root.trim_end_matches('/'))
}

/// 状态码是 2xx 时解析响应体，否则把错误带上服务端说的原因。
fn json_ok(response: &Response) -> anyhow::Result<Value> {
    if !(200..300).contains(&response.status) {
        let body: Option<Value> = serde_json::from_str(&response.body).ok();
        let reason = body
            .as_ref()
            .and_then(|body| body.pointer("/error/message").and_then(Value::as_str))
            .unwrap_or(response.body.trim());
        bail!("HTTP {}: {reason}", response.status);
    }
    serde_json::from_str(&response.body).context("the response is not JSON")
}

fn field<'a>(value: &'a Value, path: &[&str]) -> anyhow::Result<&'a str> {
    let pointer: String = path.iter().map(|key| format!("/{key}")).collect();
    value.pointer(&pointer).and_then(Value::as_str).with_context(|| format!("the response has no {pointer}"))
}

/// 豆包要的每次请求不同的 UUID 形式的编号。
fn request_id() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let n = nanos ^ (u128::from(std::process::id()) << 96);
    format!(
        "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
        (n >> 96) as u32,
        (n >> 80) as u16,
        (n >> 68) as u16 & 0xfff,
        (n >> 56) as u16 & 0xfff,
        n as u64 & 0xffff_ffff_ffff
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, api_status: &str, body: &str) -> Response {
        Response { status, api_status: api_status.to_owned(), body: body.to_owned() }
    }

    #[test]
    fn doubao_hotwords_go_into_the_corpus_context_as_a_json_string() {
        let body = doubao_body(b"RIFF", &["runode".to_owned()]);
        let context: Value = serde_json::from_str(body["request"]["corpus"]["context"].as_str().unwrap()).unwrap();
        assert_eq!(context, json!({"hotwords": [{"word": "runode"}]}));
        assert_eq!(body["audio"]["data"], "UklGRg==");
        assert!(doubao_body(b"", &[])["request"].get("corpus").is_none());
    }

    #[test]
    fn doubao_status_codes() {
        let ok = response(200, "20000000", r#"{"result":{"text":" 你好 "}}"#);
        assert_eq!(doubao_text(&ok).unwrap(), "你好");
        assert_eq!(doubao_text(&response(200, "20000003", "{}")).unwrap(), "");
        assert!(doubao_text(&response(200, "45000001", "bad")).unwrap_err().to_string().contains("45000001"));
        assert!(doubao_text(&response(401, "", "denied")).is_err());
    }

    #[test]
    fn openai_errors_carry_the_server_message() {
        let error = json_ok(&response(401, "", r#"{"error":{"message":"Incorrect API key"}}"#)).unwrap_err();
        assert_eq!(error.to_string(), "HTTP 401: Incorrect API key");
    }

    #[test]
    fn endpoints_tolerate_a_trailing_slash() {
        assert_eq!(endpoint("https://x/v1/", "chat/completions"), "https://x/v1/chat/completions");
    }

    #[test]
    fn request_ids_look_like_uuids() {
        let id = request_id();
        assert_eq!(id.len(), 36);
        assert_eq!(id.matches('-').count(), 4);
    }
}
