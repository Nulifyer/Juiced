//! Shared speech gateway for streaming voice protocols. Each call takes the same
//! admission, alias and loading-policy path as the HTTP audio endpoints.

use crate::{
    ApiError, audio,
    config::{Operation, Policy},
    supervisor::{Manager, RequestLease},
};
use axum::{body::Bytes, http::StatusCode};
use std::sync::Arc;

pub const MAX_TRANSCRIPTION_SAMPLES: usize = (crate::speech::MAX_UPLOAD_BYTES - 44) / 2;

pub async fn models(manager: &Manager, operation: Operation) -> Vec<String> {
    manager
        .catalog()
        .await
        .into_iter()
        .filter(|model| {
            model.policy != Policy::Disabled
                && model.downloaded
                && model.operations.contains(&operation)
        })
        .map(|model| model.id)
        .collect()
}

pub fn language(requested: Option<&str>) -> String {
    let code = requested
        .unwrap_or("")
        .split(['-', '_'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if (2..=3).contains(&code.len()) && code.bytes().all(|byte| byte.is_ascii_lowercase()) {
        code
    } else {
        "auto".to_owned()
    }
}

fn transfer_error(lease: &mut RequestLease, error: reqwest::Error) -> ApiError {
    let timeout = error.is_timeout();
    lease.finish(if timeout { "timeout" } else { "upstream_error" }, None);
    ApiError::new(
        if timeout {
            StatusCode::GATEWAY_TIMEOUT
        } else {
            StatusCode::BAD_GATEWAY
        },
        if timeout {
            "upstream_timeout"
        } else {
            "upstream_unavailable"
        },
        "The speech worker transfer failed. Retry or inspect the selected worker.",
    )
}

/// `samples` are 16 kHz mono; whisper.cpp decodes the WAV container itself.
pub async fn transcribe(
    manager: &Arc<Manager>,
    model: &str,
    samples: &[i16],
    language: String,
) -> Result<String, ApiError> {
    if samples.is_empty() || samples.len() > MAX_TRANSCRIPTION_SAMPLES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "transcription_audio_limit",
            "Send between one sample and the bounded transcription duration of audio.",
        ));
    }
    let request = crate::speech::Transcription {
        model: model.to_owned(),
        file: Bytes::from(audio::wav(samples, audio::WHISPER_RATE)),
        filename: "audio.wav".to_owned(),
        response_format: crate::speech::ResponseFormat::Json,
        temperature: 0.0,
        language,
        prompt: String::new(),
    };
    let (base, mut lease) = manager
        .begin_queued(
            model,
            Operation::Transcription,
            crate::admission::Priority::Interactive,
        )
        .await?;
    let (path, form) = crate::speech::worker_request(lease.adapter(), request)
        .inspect_err(|_| lease.finish("rejected", Some(400)))?;
    lease.speech_dispatched();
    let mut upstream = manager
        .client
        .post(format!("{base}{path}"))
        .multipart(form)
        .timeout(manager.request_timeout)
        .send()
        .await
        .map_err(|error| transfer_error(&mut lease, error))?;
    let status = upstream.status();
    let mut output = Vec::new();
    while let Some(chunk) = upstream
        .chunk()
        .await
        .map_err(|error| transfer_error(&mut lease, error))?
    {
        if output.len().saturating_add(chunk.len()) > crate::speech::MAX_RESPONSE_BYTES {
            lease.finish("upstream_error", Some(status.as_u16()));
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "transcription_response_limit",
                "The speech response exceeded its bounded result size.",
            ));
        }
        output.extend_from_slice(&chunk);
    }
    lease.speech_reply_eof();
    if !status.is_success() {
        lease.finish("upstream_error", Some(status.as_u16()));
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "transcription_rejected",
            "The speech worker rejected this audio request. Check its format and the selected worker.",
        ));
    }
    let malformed = || {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "invalid_transcription_response",
            "The speech worker returned a malformed or oversized response.",
        )
    };
    let text =
        crate::speech::validate_response(Bytes::from(output), crate::speech::ResponseFormat::Json)
            .ok()
            .and_then(|validated| {
                serde_json::from_slice::<serde_json::Value>(&validated.bytes).ok()
            })
            .and_then(|value| {
                value["text"]
                    .as_str()
                    .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
            });
    match text {
        Some(text) => {
            lease.finish("completed", Some(status.as_u16()));
            Ok(text)
        }
        None => {
            lease.finish("upstream_error", Some(status.as_u16()));
            Err(malformed())
        }
    }
}

pub async fn listed_voices(manager: &Manager, model: &str) -> Vec<String> {
    manager
        .voices(Some(model))
        .await
        .ok()
        .and_then(|rows| rows.into_iter().next())
        .and_then(|row| row["voices"].as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|voice| voice.as_str().map(str::to_owned))
        .collect()
}

/// Kokoro's reference voice when the pack has it; packs list voices alphabetically.
pub fn default_voice(listed: &[String]) -> String {
    const PREFERRED: &str = "af_heart";
    if listed.is_empty() || listed.iter().any(|voice| voice == PREFERRED) {
        PREFERRED.to_owned()
    } else {
        listed[0].clone()
    }
}

pub async fn synthesize(
    manager: &Arc<Manager>,
    model: &str,
    text: &str,
    voice: &str,
) -> Result<audio::Decoded, ApiError> {
    if text.trim().is_empty() || text.len() > crate::media::MAX_SPEECH_INPUT_BYTES {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_speech_request",
            "Send nonempty text within the bounded speech input size.",
        ));
    }
    if !crate::media::valid_voice_selection(voice) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_speech_request",
            "voice must name a voice, or a blend like af_sarah.4+af_nicole.6.",
        ));
    }
    let request = crate::media::SpeechRequest {
        model: model.to_owned(),
        input: text.to_owned(),
        voice: voice.to_owned(),
        format: crate::media::AudioFormat::Wav,
        speed: 1.0,
    };
    let (bytes, mut lease) = crate::http::media_exchange(
        manager,
        model,
        Operation::Speech,
        crate::admission::Priority::Interactive,
        "/v1/audio/speech",
        request.upstream(),
        crate::media::MAX_SPEECH_RESPONSE_BYTES,
    )
    .await?;
    match audio::decode_wav(&bytes) {
        Some(decoded) => {
            lease.finish("completed", Some(200));
            Ok(decoded)
        }
        None => {
            lease.finish("upstream_error", Some(200));
            Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "invalid_speech_response",
                "The speech worker returned audio that is not PCM WAV.",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn languages_normalize_to_whisper_codes() {
        assert_eq!(super::language(Some("en-US")), "en");
        assert_eq!(super::language(Some("pt_BR")), "pt");
        assert_eq!(super::language(Some("DE")), "de");
        assert_eq!(super::language(None), "auto");
        assert_eq!(super::language(Some("english")), "auto");
        assert_eq!(super::language(Some("e1")), "auto");
    }

    #[test]
    fn default_voice_prefers_the_reference_voice() {
        let listed = |names: &[&str]| {
            names
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            super::default_voice(&listed(&["af_alloy", "af_heart"])),
            "af_heart"
        );
        assert_eq!(
            super::default_voice(&listed(&["bf_emma", "bm_george"])),
            "bf_emma"
        );
        assert_eq!(super::default_voice(&[]), "af_heart");
    }
}
