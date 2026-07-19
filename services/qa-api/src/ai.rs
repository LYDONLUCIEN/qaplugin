// Multi-provider LLM client with streaming.
//
// Supports:
//   - Anthropic Messages API (Claude)
//   - OpenAI-compatible chat completions (OpenAI / DeepSeek / Qwen / GLM /
//     Kimi / Doubao / OpenRouter / ... any OpenAI-shaped endpoint)
//
// Vision payloads are emitted in the correct shape per provider.
//
// Config via env:
//   LLM_PROVIDER   = anthropic | openai | deepseek | qwen | glm | kimi |
//                    openrouter | doubao  (case-insensitive; default: auto)
//   LLM_BASE_URL   = override base, e.g. https://api.deepseek.com
//   LLM_API_KEY    = key for the chosen provider
//                    (falls back to ANTHROPIC_API_KEY / OPENAI_API_KEY /
//                     DEEPSEEK_API_KEY / DASHSCOPE_API_KEY / MOONSHOT_API_KEY /
//                     ZHIPU_API_KEY / OPENROUTER_API_KEY)
//   LLM_MODEL      = model id (default depends on provider)
//   LLM_MAX_TOKENS = (optional, default 1024)

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use futures_util::Stream;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use reqwest::Client;
use serde::Serialize;
use std::pin::Pin;

const DEFAULT_MAX_TOKENS: u32 = 1024;
// Qwen's OpenAI-compatible API limits a Base64 image to 10 MB. Keep a
// margin for provider differences and JSON/Data URL overhead.
const TARGET_BASE64_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    OpenAiCompatible,
}

#[derive(Clone, Debug)]
pub struct LlmConfig {
    pub provider: Provider,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub max_tokens: u32,
}

impl LlmConfig {
    pub fn from_env() -> Result<Self> {
        let provider_str = std::env::var("LLM_PROVIDER")
            .unwrap_or_default()
            .to_lowercase();

        // Resolve API key from any of the supported env vars.
        let api_key = [
            "LLM_API_KEY",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "DEEPSEEK_API_KEY",
            "DASHSCOPE_API_KEY",
            "MOONSHOT_API_KEY",
            "ZHIPU_API_KEY",
            "OPENROUTER_API_KEY",
        ]
        .iter()
        .find_map(|n| std::env::var(n).ok().filter(|s| !s.is_empty()))
        .ok_or_else(|| {
            anyhow!(
                "No LLM API key set. Set one of: LLM_API_KEY, ANTHROPIC_API_KEY, \
                 OPENAI_API_KEY, DEEPSEEK_API_KEY, DASHSCOPE_API_KEY, MOONSHOT_API_KEY, \
                 ZHIPU_API_KEY, OPENROUTER_API_KEY"
            )
        })?;

        // Map named provider → enum.
        let (provider, default_base, default_model) = match provider_str.as_str() {
            "anthropic" | "claude" => (
                Provider::Anthropic,
                "https://api.anthropic.com",
                "claude-sonnet-4-6",
            ),
            "openai" => (
                Provider::OpenAiCompatible,
                "https://api.openai.com",
                "gpt-4o",
            ),
            "deepseek" => (
                Provider::OpenAiCompatible,
                "https://api.deepseek.com",
                "deepseek-chat",
            ),
            "qwen" | "dashscope" | "tongyi" => (
                Provider::OpenAiCompatible,
                "https://dashscope.aliyuncs.com/compatible-mode",
                "qwen3-vl-plus",
            ),
            "glm" | "zhipu" => (
                Provider::OpenAiCompatible,
                "https://open.bigmodel.cn/api/paas/v4",
                "glm-4v",
            ),
            "kimi" | "moonshot" => (
                Provider::OpenAiCompatible,
                "https://api.moonshot.cn",
                "moonshot-v1-8k-vision-preview",
            ),
            "doubao" => (
                Provider::OpenAiCompatible,
                "https://ark.cn-beijing.volces.com/api",
                "doubao-vision-pro-32k",
            ),
            "openrouter" => (
                Provider::OpenAiCompatible,
                "https://openrouter.ai/api",
                "anthropic/claude-sonnet-4",
            ),
            "" => {
                // Auto-detect: sk-ant-* → Anthropic, otherwise OpenAI-compat.
                if api_key.starts_with("sk-ant") {
                    (
                        Provider::Anthropic,
                        "https://api.anthropic.com",
                        "claude-sonnet-4-6",
                    )
                } else {
                    (
                        Provider::OpenAiCompatible,
                        "https://api.openai.com",
                        "gpt-4o",
                    )
                }
            }
            other => {
                return Err(anyhow!(
                    "Unknown LLM_PROVIDER='{other}'. Valid: anthropic, openai, deepseek, \
                 qwen, glm, kimi, doubao, openrouter"
                ))
            }
        };

        let base_url = std::env::var("LLM_BASE_URL").unwrap_or_else(|_| default_base.into());
        let model = std::env::var("LLM_MODEL").unwrap_or_else(|_| default_model.into());
        let max_tokens = std::env::var("LLM_MAX_TOKENS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_MAX_TOKENS);

        log::info!("LLM config: provider={provider:?} model={model} base={base_url}");

        Ok(LlmConfig {
            provider,
            base_url,
            api_key,
            model,
            max_tokens,
        })
    }
}

type BoxStream = Pin<Box<dyn Stream<Item = Result<String>> + Send>>;

pub async fn stream_vision(
    cfg: &LlmConfig,
    prompt: &str,
    image_b64: &str,
    image_mime: &str,
) -> Result<BoxStream> {
    let client = Client::builder().user_agent("qa-snapshot/0.1").build()?;
    let image = prepare_image(image_b64, image_mime)?;
    match cfg.provider {
        Provider::Anthropic => Ok(Box::pin(
            anthropic_stream(&client, cfg, prompt, &image).await?,
        )),
        Provider::OpenAiCompatible => {
            Ok(Box::pin(openai_stream(&client, cfg, prompt, &image).await?))
        }
    }
}

pub(crate) struct PreparedImage {
    pub base64: String,
    pub mime_type: String,
}

pub(crate) fn prepare_image(image_b64: &str, image_mime: &str) -> Result<PreparedImage> {
    if image_b64.len() <= TARGET_BASE64_BYTES {
        return Ok(PreparedImage {
            base64: image_b64.to_string(),
            mime_type: image_mime.to_string(),
        });
    }

    let bytes = STANDARD
        .decode(image_b64)
        .context("invalid screenshot Base64")?;
    let original = image::load_from_memory(&bytes).context("invalid screenshot image")?;

    // Preserve enough resolution for UI text while reducing full-screen Retina
    // screenshots to a provider-friendly request size.
    for (max_side, quality) in [(2560, 88), (2048, 82), (1600, 76)] {
        let resized = original.resize(max_side, max_side, FilterType::Lanczos3);
        let rgb = resized.to_rgb8();
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, quality)
            .encode(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                image::ColorType::Rgb8.into(),
            )
            .context("failed to compress screenshot")?;
        let encoded = STANDARD.encode(jpeg);
        if encoded.len() <= TARGET_BASE64_BYTES {
            log::info!(
                "compressed vision image from {} to {} Base64 bytes ({}x{}, JPEG q{})",
                image_b64.len(),
                encoded.len(),
                rgb.width(),
                rgb.height(),
                quality
            );
            return Ok(PreparedImage {
                base64: encoded,
                mime_type: "image/jpeg".to_string(),
            });
        }
    }

    Err(anyhow!(
        "screenshot remains larger than {} Base64 bytes after compression",
        TARGET_BASE64_BYTES
    ))
}

// ─────────────────────────────────────────────────────────────────────────
// Anthropic Messages API
// ─────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct AnthropicReq<'a> {
    model: &'a str,
    max_tokens: u32,
    stream: bool,
    messages: [AnthropicMessage<'a>; 1],
}

#[derive(Serialize)]
struct AnthropicMessage<'a> {
    role: &'a str,
    content: [serde_json::Value; 2],
}

async fn anthropic_stream(
    client: &Client,
    cfg: &LlmConfig,
    prompt: &str,
    image: &PreparedImage,
) -> Result<impl Stream<Item = Result<String>> + Send> {
    let body = AnthropicReq {
        model: &cfg.model,
        max_tokens: cfg.max_tokens,
        stream: true,
        messages: [AnthropicMessage {
            role: "user",
            content: [
                serde_json::json!({
                    "type": "image",
                    "source": { "type": "base64", "media_type": &image.mime_type, "data": &image.base64 }
                }),
                serde_json::json!({ "type": "text", "text": prompt }),
            ],
        }],
    };

    let url = format!("{}/v1/messages", cfg.base_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header("x-api-key", &cfg.api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .await?;
    let resp = check_status(resp).await?;
    Ok(parse_sse(resp.bytes_stream(), anthropic_extract))
}

fn anthropic_extract(v: &serde_json::Value) -> Option<String> {
    if v.get("type").and_then(|x| x.as_str()) == Some("content_block_delta") {
        v.pointer("/delta/text")
            .and_then(|x| x.as_str())
            .map(String::from)
    } else {
        None
    }
}

// ─────────────────────────────────────────────────────────────────────────
// OpenAI-compatible chat completions
// ─────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct OpenAiReq<'a> {
    model: &'a str,
    max_tokens: u32,
    stream: bool,
    messages: [OpenAiMessage<'a>; 1],
}

#[derive(Serialize)]
struct OpenAiMessage<'a> {
    role: &'a str,
    content: [serde_json::Value; 2],
}

async fn openai_stream(
    client: &Client,
    cfg: &LlmConfig,
    prompt: &str,
    image: &PreparedImage,
) -> Result<impl Stream<Item = Result<String>> + Send> {
    let body = OpenAiReq {
        model: &cfg.model,
        max_tokens: cfg.max_tokens,
        stream: true,
        messages: [OpenAiMessage {
            role: "user",
            content: [
                serde_json::json!({
                    "type": "image_url",
                    "image_url": { "url": format!("data:{};base64,{}", image.mime_type, image.base64) }
                }),
                serde_json::json!({ "type": "text", "text": prompt }),
            ],
        }],
    };

    let url = chat_completions_url(&cfg.base_url);
    let resp = client
        .post(&url)
        .header("authorization", format!("Bearer {}", cfg.api_key))
        .json(&body)
        .send()
        .await?;
    let resp = check_status(resp).await?;
    Ok(parse_sse(resp.bytes_stream(), openai_extract))
}

fn chat_completions_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/chat/completions")
    } else {
        format!("{base}/v1/chat/completions")
    }
}

fn openai_extract(v: &serde_json::Value) -> Option<String> {
    if let Some(err) = v.get("error") {
        return Some(format!(
            "[api error: {}]",
            err.get("message")
                .and_then(|x| x.as_str())
                .unwrap_or("unknown")
        ));
    }
    v.pointer("/choices/0/delta/content")
        .and_then(|x| x.as_str())
        .map(String::from)
}

// ─────────────────────────────────────────────────────────────────────────
// Shared SSE parser
// ─────────────────────────────────────────────────────────────────────────

async fn check_status(resp: reqwest::Response) -> Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    Err(anyhow!("api {status}: {text}"))
}

fn parse_sse<S, F>(bytes: S, extract: F) -> impl Stream<Item = Result<String>> + Send
where
    S: Stream<Item = reqwest::Result<bytes::Bytes>> + Unpin + Send + 'static,
    F: Fn(&serde_json::Value) -> Option<String> + Send + Sync + 'static,
{
    use futures_util::StreamExt;
    async_stream::try_stream! {
        let mut bytes = bytes;
        let mut buf = Vec::new();
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.context("network read")?;
            buf.extend_from_slice(&chunk);
            while let Some((idx, separator_len)) = sse_boundary(&buf) {
                let event = String::from_utf8_lossy(&buf[..idx]).into_owned();
                buf.drain(..idx + separator_len);
                for line in event.lines() {
                    let Some(data) = line.strip_prefix("data:") else { continue };
                    let data = data.trim();
                    if data == "[DONE]" { return; }
                    let Ok(v): std::result::Result<serde_json::Value, _> = serde_json::from_str(data) else { continue };
                    if let Some(s) = extract(&v) {
                        yield s;
                    }
                    if v.get("type").and_then(|x| x.as_str()) == Some("error") {
                        let msg = v.pointer("/error/message").and_then(|x| x.as_str()).unwrap_or("api error");
                        Err(anyhow!("stream error: {msg}"))?;
                    }
                }
            }
        }
    }
}

fn sse_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n");
    let crlf = buffer.windows(4).position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(left), Some(right)) if left <= right => Some((left, 2)),
        (Some(_), Some(right)) => Some((right, 4)),
        (Some(index), None) => Some((index, 2)),
        (None, Some(index)) => Some((index, 4)),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{chat_completions_url, openai_extract, parse_sse, sse_boundary};
    use bytes::Bytes;
    use futures_util::{stream, StreamExt};

    #[test]
    fn accepts_base_urls_with_or_without_v1() {
        assert_eq!(
            chat_completions_url("https://dashscope.aliyuncs.com/compatible-mode"),
            "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://workspace.example/compatible-mode/v1"),
            "https://workspace.example/compatible-mode/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://workspace.example/compatible-mode/v1/chat/completions"),
            "https://workspace.example/compatible-mode/v1/chat/completions"
        );
    }

    #[test]
    fn detects_lf_and_crlf_sse_boundaries() {
        assert_eq!(sse_boundary(b"data: one\n\nrest"), Some((9, 2)));
        assert_eq!(sse_boundary(b"data: two\r\n\r\nrest"), Some((9, 4)));
        assert_eq!(sse_boundary(b"data: partial"), None);
    }

    #[tokio::test]
    async fn streams_utf8_deltas_split_across_network_chunks() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"你\"}}]}\r\n\r\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"好\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let first_split = body.find('你').unwrap() + 1;
        let second_split = body.find('好').unwrap() + 2;
        let chunks: Vec<reqwest::Result<Bytes>> = vec![
            Ok(Bytes::copy_from_slice(&body.as_bytes()[..first_split])),
            Ok(Bytes::copy_from_slice(
                &body.as_bytes()[first_split..second_split],
            )),
            Ok(Bytes::copy_from_slice(&body.as_bytes()[second_split..])),
        ];

        let deltas = parse_sse(stream::iter(chunks), openai_extract);
        futures_util::pin_mut!(deltas);
        let mut collected = Vec::new();
        while let Some(delta) = deltas.next().await {
            collected.push(delta.unwrap());
        }

        assert_eq!(collected, ["你", "好"]);
    }
}
