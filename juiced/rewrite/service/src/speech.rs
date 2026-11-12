use crate::ApiError;
use axum::{
    body::Bytes,
    extract::{
        Multipart,
        multipart::{Field, MultipartError},
    },
    http::StatusCode,
};
use reqwest::multipart::{Form, Part};
use serde::{
    Deserialize, Serialize,
    de::{self, IgnoredAny, MapAccess, Visitor},
};
use std::fmt;

pub const MAX_UPLOAD_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PROMPT_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormat {
    #[default]
    Json,
    VerboseJson,
    Text,
    Srt,
    Vtt,
}
impl ResponseFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::VerboseJson => "verbose_json",
            Self::Text => "text",
            Self::Srt => "srt",
            Self::Vtt => "vtt",
        }
    }
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Json | Self::VerboseJson => "application/json",
            Self::Text | Self::Srt | Self::Vtt => "text/plain; charset=utf-8",
        }
    }
    fn parse(value: &str) -> Result<Self, ApiError> {
        match value {
            "json" => Ok(Self::Json),
            "verbose_json" => Ok(Self::VerboseJson),
            "text" => Ok(Self::Text),
            "srt" => Ok(Self::Srt),
            "vtt" => Ok(Self::Vtt),
            _ => Err(invalid(
                "response_format must be json, verbose_json, text, srt, or vtt.",
            )),
        }
    }
}

pub struct Transcription {
    pub model: String,
    pub file: Bytes,
    pub filename: String,
    pub response_format: ResponseFormat,
    pub temperature: f64,
    pub language: String,
    pub prompt: String,
}
impl Transcription {
    fn file_part(&self) -> Part {
        let mime = if is_wav(&self.file) {
            "audio/wav"
        } else {
            "application/octet-stream"
        };
        Part::stream_with_length(
            reqwest::Body::from(self.file.clone()),
            self.file.len() as u64,
        )
        .file_name(self.filename.clone())
        .mime_str(mime)
        .expect("The adapter's constant MIME types are valid")
    }
    pub fn form(self) -> Form {
        let file = self.file_part();
        let temperature = if self.temperature == 0.0 {
            "0.0".to_owned()
        } else {
            self.temperature.to_string()
        };
        Form::new()
            .part("file", file)
            .text("response_format", self.response_format.as_str())
            .text("temperature", temperature)
            .text("language", self.language)
            .text("prompt", self.prompt)
    }
}

/// The selected worker's request contract; whisper.cpp receives every OpenAI field.
pub fn worker_request(
    adapter: crate::config::Provider,
    mut request: Transcription,
) -> Result<(&'static str, Form), ApiError> {
    use crate::config::Provider;
    let unsupported = |message: &'static str| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "unsupported_transcription_option",
            message,
        )
    };
    let format = request.response_format;
    let path = adapter.transcription_path();
    match adapter {
        Provider::Parakeet | Provider::Asr4all if !is_wav(&request.file) => {
            Err(unsupported("This speech engine accepts WAV uploads only."))
        }
        Provider::Parakeet if matches!(format, ResponseFormat::Srt | ResponseFormat::Vtt) => Err(
            unsupported("parakeet.cpp supports response_format json, verbose_json or text."),
        ),
        Provider::Asr4all if !matches!(format, ResponseFormat::Json | ResponseFormat::Text) => Err(
            unsupported("FUTO asr4all supports response_format json or text."),
        ),
        Provider::Asr4all if !matches!(request.language.as_str(), "auto" | "en") => {
            Err(unsupported("FUTO asr4all transcribes English only."))
        }
        Provider::Asr4all if !request.prompt.is_empty() || request.temperature != 0.0 => Err(
            unsupported("FUTO asr4all supports neither prompt biasing nor temperature sampling."),
        ),
        Provider::Parakeet | Provider::Moonshine | Provider::Asr4all => {
            // These engines read only 16-bit PCM WAV; float, 24-bit or stereo uploads are rewritten.
            if adapter != Provider::Moonshine
                && let Some(file) = pcm16_wav(&request.file)
            {
                request.file = file;
            }
            let language = (adapter == Provider::Asr4all).then(|| request.language.clone());
            let mut form = Form::new()
                .part("file", request.file_part())
                .text("response_format", format.as_str());
            if let Some(language) = language {
                form = form.text("language", language);
            }
            Ok((path, form))
        }
        _ => Ok((path, request.form())),
    }
}

pub struct ValidatedResponse {
    pub bytes: Bytes,
    pub content_type: &'static str,
}

fn invalid(message: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_transcription", message)
}
fn field_limit() -> ApiError {
    ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "transcription_field_limit",
        "A transcription field exceeds its byte limit.",
    )
}
fn multipart_error(error: MultipartError) -> ApiError {
    let status = if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        StatusCode::PAYLOAD_TOO_LARGE
    } else {
        StatusCode::BAD_REQUEST
    };
    ApiError::new(
        status,
        "invalid_multipart",
        "The transcription multipart body is malformed or exceeds its size limit.",
    )
}
fn upstream_error() -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "invalid_transcription_response",
        "The speech worker returned a malformed or oversized response.",
    )
}
fn pcm16_wav(file: &[u8]) -> Option<Bytes> {
    let decoded = crate::audio::decode_wav(file)?;
    let samples = crate::audio::resample(&decoded.samples, decoded.rate, 16_000);
    Some(Bytes::from(crate::audio::wav(&samples, 16_000)))
}

fn is_wav(bytes: &[u8]) -> bool {
    bytes.len() >= 12
        && matches!(&bytes[..4], b"RIFF" | b"RIFX" | b"RF64" | b"BW64")
        && &bytes[8..12] == b"WAVE"
}
fn filename(value: Option<&str>) -> String {
    let basename = value
        .unwrap_or("audio")
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("audio")
        .trim_matches('.');
    let mut result: String = basename
        .chars()
        .take(128)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if result.is_empty() {
        result.push_str("audio");
    }
    let stem = result.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        result.insert_str(0, "audio_");
        result.truncate(128);
    }
    result
}
async fn field_bytes(
    field: &mut Field<'_>,
    limit: usize,
    total: &mut usize,
) -> Result<Bytes, ApiError> {
    let first = field
        .chunk()
        .await
        .map_err(multipart_error)?
        .unwrap_or_default();
    if first.len() > limit || first.len() > MAX_UPLOAD_BYTES.saturating_sub(*total) {
        return Err(field_limit());
    }
    *total += first.len();
    let Some(second) = field.chunk().await.map_err(multipart_error)? else {
        return Ok(first);
    };
    if second.len() > limit.saturating_sub(first.len())
        || second.len() > MAX_UPLOAD_BYTES.saturating_sub(*total)
    {
        return Err(field_limit());
    }
    *total += second.len();
    let mut buffer = Vec::with_capacity(first.len() + second.len());
    buffer.extend_from_slice(&first);
    buffer.extend_from_slice(&second);
    while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
        if chunk.len() > limit.saturating_sub(buffer.len())
            || chunk.len() > MAX_UPLOAD_BYTES.saturating_sub(*total)
        {
            return Err(field_limit());
        }
        *total += chunk.len();
        buffer.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(buffer))
}
fn text(bytes: Bytes) -> Result<String, ApiError> {
    let value = std::str::from_utf8(&bytes)
        .map_err(|_| invalid("Transcription text fields must be valid UTF-8."))?;
    if value.contains('\0') {
        return Err(invalid(
            "Transcription text fields cannot contain NUL bytes.",
        ));
    }
    Ok(value.to_owned())
}

pub async fn parse(mut multipart: Multipart) -> Result<Transcription, ApiError> {
    let mut seen = [false; 6];
    let mut model = None;
    let mut file = None;
    let mut file_name = "audio".to_owned();
    let mut response_format = ResponseFormat::Json;
    let mut temperature = 0.0;
    let mut language = "auto".to_owned();
    let mut prompt = String::new();
    let mut total = 0;
    while let Some(mut field) = multipart.next_field().await.map_err(multipart_error)? {
        let (index, limit) = match field.name() {
            Some("model") => (0, 256),
            Some("file") => (1, MAX_UPLOAD_BYTES),
            Some("response_format") => (2, 32),
            Some("temperature") => (3, 32),
            Some("language") => (4, 32),
            Some("prompt") => (5, MAX_PROMPT_BYTES),
            _ => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "unsupported_parameter",
                    "Use only supported transcription form fields.",
                ));
            }
        };
        if std::mem::replace(&mut seen[index], true) {
            return Err(invalid("Transcription form fields must not be repeated."));
        }
        if index != 1 && field.file_name().is_some() {
            return Err(invalid("Transcription text fields cannot be file uploads."));
        }
        if index == 1 {
            file_name = filename(field.file_name());
        }
        let value = field_bytes(&mut field, limit, &mut total).await?;
        match index {
            0 => {
                let value = text(value)?;
                if value.trim().is_empty() || value.chars().any(char::is_control) {
                    return Err(invalid("A nonempty registered model ID is required."));
                }
                model = Some(value);
            }
            1 => {
                if value.is_empty() {
                    return Err(invalid("A nonempty audio file is required."));
                }
                file = Some(value);
            }
            2 => response_format = ResponseFormat::parse(&text(value)?)?,
            3 => {
                let value = text(value)?.trim().parse::<f64>().map_err(|_| {
                    invalid("temperature must be a finite number from zero to one.")
                })?;
                if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                    return Err(invalid(
                        "temperature must be a finite number from zero to one.",
                    ));
                }
                temperature = if value == 0.0 { 0.0 } else { value };
            }
            4 => {
                let value = text(value)?;
                if value != "auto"
                    && !((2..=3).contains(&value.len())
                        && value.bytes().all(|byte| byte.is_ascii_alphabetic()))
                {
                    return Err(invalid(
                        "language must be auto or a two- or three-letter language code.",
                    ));
                }
                language = value.to_ascii_lowercase();
            }
            5 => prompt = text(value)?,
            _ => unreachable!(),
        }
    }
    Ok(Transcription {
        model: model.ok_or_else(|| invalid("A registered model ID is required."))?,
        file: file.ok_or_else(|| invalid("A nonempty audio file is required."))?,
        filename: file_name,
        response_format,
        temperature,
        language,
        prompt,
    })
}

#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum ResponseKey {
    Text,
    Error,
    #[serde(other)]
    Other,
}
struct ResponseText;
impl<'de> Deserialize<'de> for ResponseText {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TextVisitor;
        impl Visitor<'_> for TextVisitor {
            type Value = ResponseText;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a transcription string")
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(ResponseText)
            }
        }
        deserializer.deserialize_str(TextVisitor)
    }
}
struct JsonResponse;
impl<'de> Deserialize<'de> for JsonResponse {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = JsonResponse;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a transcription object containing text")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut text = false;
                let mut error = false;
                while let Some(key) = map.next_key::<ResponseKey>()? {
                    match key {
                        ResponseKey::Text => {
                            if text {
                                return Err(de::Error::duplicate_field("text"));
                            }
                            map.next_value::<ResponseText>()?;
                            text = true;
                        }
                        ResponseKey::Error => {
                            map.next_value::<IgnoredAny>()?;
                            error = true;
                        }
                        ResponseKey::Other => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                if !text || error {
                    return Err(de::Error::custom("Invalid transcription object"));
                }
                Ok(JsonResponse)
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}

pub fn validate_response(
    bytes: Bytes,
    format: ResponseFormat,
) -> Result<ValidatedResponse, ApiError> {
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(upstream_error());
    }
    match format {
        ResponseFormat::Json | ResponseFormat::VerboseJson => {
            serde_json::from_slice::<JsonResponse>(&bytes).map_err(|_| upstream_error())?;
        }
        ResponseFormat::Text | ResponseFormat::Srt | ResponseFormat::Vtt => {
            std::str::from_utf8(&bytes).map_err(|_| upstream_error())?;
        }
    }
    Ok(ValidatedResponse {
        bytes,
        content_type: format.content_type(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        extract::FromRequest,
        http::{Request, header},
    };
    use futures_util::StreamExt;

    fn part(output: &mut Vec<u8>, name: &str, filename: Option<&str>, bytes: &[u8]) {
        output.extend_from_slice(
            format!(
                "--wire\r\nContent-Disposition: form-data; name=\"{name}\"{}\r\n\r\n",
                filename
                    .map(|name| format!("; filename=\"{name}\""))
                    .unwrap_or_default()
            )
            .as_bytes(),
        );
        output.extend_from_slice(bytes);
        output.extend_from_slice(b"\r\n");
    }
    async fn request(
        parts: &[(&str, Option<&str>, &[u8])],
        fragmented: bool,
    ) -> Result<Transcription, ApiError> {
        let mut bytes = Vec::new();
        for (name, filename, value) in parts {
            part(&mut bytes, name, *filename, value);
        }
        bytes.extend_from_slice(b"--wire--\r\n");
        let body = if fragmented {
            let chunks: Vec<_> = bytes
                .chunks(3)
                .map(|chunk| Ok::<_, std::io::Error>(Bytes::copy_from_slice(chunk)))
                .collect();
            Body::from_stream(futures_util::stream::iter(chunks))
        } else {
            Body::from(bytes)
        };
        let request = Request::builder()
            .header(header::CONTENT_TYPE, "multipart/form-data; boundary=wire")
            .body(body)
            .unwrap();
        let multipart = Multipart::from_request(request, &()).await.unwrap();
        parse(multipart).await
    }
    #[tokio::test]
    async fn fragmented_wire_parsing_has_explicit_defaults_and_safe_filename() {
        let parsed = request(
            &[
                (
                    "file",
                    Some("../../private/recording.wav"),
                    b"RIFF\0\0\0\0WAVEaudio",
                ),
                ("model", None, b"speech-model"),
            ],
            true,
        )
        .await
        .unwrap();
        assert_eq!(parsed.model, "speech-model");
        assert_eq!(parsed.filename, "recording.wav");
        assert_eq!(parsed.response_format, ResponseFormat::Json);
        assert_eq!(parsed.temperature, 0.0);
        assert_eq!(parsed.language, "auto");
        assert!(parsed.prompt.is_empty());
        assert!(is_wav(&parsed.file));
    }
    #[tokio::test]
    async fn independent_requests_reset_native_parameters_and_forward_no_public_model() {
        let custom = request(
            &[
                ("model", None, b"m"),
                ("file", Some("clip.bin"), b"audio"),
                ("language", None, b"DE"),
                ("temperature", None, b"0.25"),
                ("response_format", None, b"verbose_json"),
                ("prompt", None, b"custom hint"),
            ],
            false,
        )
        .await
        .unwrap();
        assert_eq!(custom.language, "de");
        assert_eq!(custom.temperature, 0.25);
        let default = request(&[("model", None, b"m"), ("file", None, b"audio")], false)
            .await
            .unwrap();
        let form = default.form();
        let boundary = form.boundary().to_owned();
        let req = Request::builder()
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from_stream(form.into_stream()))
            .unwrap();
        let mut form = Multipart::from_request(req, &()).await.unwrap();
        let mut names = Vec::new();
        while let Some(field) = form.next_field().await.unwrap() {
            let name = field.name().unwrap().to_owned();
            if name == "file" {
                assert_eq!(field.content_type(), Some("application/octet-stream"));
            }
            let value = field.bytes().await.unwrap();
            match name.as_str() {
                "response_format" => assert_eq!(value, "json"),
                "temperature" => assert_eq!(value, "0.0"),
                "language" => assert_eq!(value, "auto"),
                "prompt" => assert!(value.is_empty()),
                "file" => assert_eq!(value, "audio"),
                _ => panic!("An unexpected native form field was forwarded"),
            }
            names.push(name);
        }
        assert_eq!(names.len(), 5);
        assert!(!names.iter().any(|name| name == "model"));
    }
    #[tokio::test]
    async fn native_file_part_retains_bytes_ownership_without_whole_vector_copy() {
        let file = Bytes::from_static(b"RIFF\0\0\0\0WAVEaudio");
        let pointer = file.as_ptr();
        let request = Transcription {
            model: "m".into(),
            file: file.clone(),
            filename: "audio.wav".into(),
            response_format: ResponseFormat::Json,
            temperature: 0.0,
            language: "auto".into(),
            prompt: String::new(),
        };
        let mut stream = request.form().into_stream();
        let mut found = false;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            if chunk == file {
                assert_eq!(chunk.as_ptr(), pointer);
                found = true;
            }
        }
        assert!(found);
    }
    #[tokio::test]
    async fn duplicate_unknown_empty_and_nonfinite_parameters_fail_explicitly() {
        for extra in [
            ("model", b"duplicate".as_slice()),
            ("stream", b"true"),
            ("timestamp_granularities[]", b"word"),
            ("temperature", b"NaN"),
            ("temperature", b"inf"),
            ("temperature", b"-1"),
            ("temperature", b"2"),
            ("language", b"en-US"),
            ("language", b"en\r\n"),
            ("response_format", b"invalid"),
        ] {
            let parts = [
                ("model", None, b"m".as_slice()),
                ("file", None, b"audio".as_slice()),
                (extra.0, None, extra.1),
            ];
            assert_eq!(
                request(&parts, false).await.err().unwrap().status,
                StatusCode::BAD_REQUEST
            );
        }
        assert!(request(&[("file", None, b"audio")], false).await.is_err());
        assert!(request(&[("model", None, b"m")], false).await.is_err());
        assert!(
            request(&[("model", None, b"m"), ("file", None, b"")], false)
                .await
                .is_err()
        );
        let oversized = vec![b'a'; MAX_PROMPT_BYTES + 1];
        assert_eq!(
            request(
                &[
                    ("model", None, b"m"),
                    ("file", None, b"audio"),
                    ("prompt", None, &oversized)
                ],
                true
            )
            .await
            .err()
            .unwrap()
            .status,
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert!(
            request(
                &[
                    ("model", None, b"m"),
                    ("file", None, b"audio"),
                    ("prompt", None, b"private\0hint")
                ],
                false
            )
            .await
            .is_err()
        );
    }
    #[test]
    fn basenames_cannot_be_paths_controls_or_reserved_device_names() {
        assert_eq!(filename(Some("C:\\private\\clip.wav")), "clip.wav");
        assert_eq!(filename(Some("../../..")), "audio");
        assert_eq!(filename(Some("CON.wav")), "audio_CON.wav");
        assert!(!filename(Some("a\r\n\"b.wav")).chars().any(char::is_control));
        assert!(filename(Some(&"界".repeat(200))).len() <= 128);
        assert!(!is_wav(b"pretend.wav"));
        let mut float = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        float.extend(16u32.to_le_bytes());
        float.extend(3u16.to_le_bytes());
        float.extend(1u16.to_le_bytes());
        float.extend(24_000u32.to_le_bytes());
        float.extend(96_000u32.to_le_bytes());
        float.extend(4u16.to_le_bytes());
        float.extend(32u16.to_le_bytes());
        float.extend(b"data");
        float.extend(96u32.to_le_bytes());
        for index in 0..24 {
            float.extend((index as f32 / 48.0).to_le_bytes());
        }
        let converted = pcm16_wav(&float).unwrap();
        let decoded = crate::audio::decode_wav(&converted).unwrap();
        assert_eq!(decoded.rate, 16_000);
        assert_eq!(&converted[20..24], &[1, 0, 1, 0]);
        assert_eq!(u16::from_le_bytes([converted[34], converted[35]]), 16);
        assert!(!decoded.samples.is_empty());
    }
    #[test]
    fn response_validation_preserves_verbose_json_and_plain_json_looking_transcripts() {
        let input = Bytes::from_static(br#"{"text":"h\u00e9llo","segments":[{"start":0,"end":1,"text":"hi"}],"language":"en"}"#);
        let pointer = input.as_ptr();
        let validated = validate_response(input.clone(), ResponseFormat::VerboseJson).unwrap();
        assert_eq!(validated.bytes.as_ptr(), pointer);
        assert_eq!(validated.bytes, input);
        assert_eq!(validated.content_type, "application/json");
        for format in [
            ResponseFormat::Text,
            ResponseFormat::Srt,
            ResponseFormat::Vtt,
        ] {
            for text in [
                "true",
                "null",
                "123",
                "{\"text\":\"still literal\"}",
                "WEBVTT\n\n00:00.000 --> 00:01.000\nλ\n",
                "",
            ] {
                let bytes = Bytes::copy_from_slice(text.as_bytes());
                assert_eq!(
                    validate_response(bytes.clone(), format).unwrap().bytes,
                    bytes
                );
            }
        }
    }
    #[test]
    fn malformed_worker_responses_are_redacted_gateway_errors() {
        for body in [
            b"not json".as_slice(),
            b"{\"text\":1}",
            b"{\"error\":\"private worker path\"}",
            b"{\"text\":\"ok\",\"error\":null}",
            b"{\"text\":\"a\",\"text\":\"b\"}",
            b"[]",
            b"null",
        ] {
            let error = validate_response(Bytes::copy_from_slice(body), ResponseFormat::Json)
                .err()
                .unwrap();
            assert_eq!(error.status, StatusCode::BAD_GATEWAY);
            assert!(!error.message.contains("private"));
        }
        assert!(validate_response(Bytes::from_static(b"\xff"), ResponseFormat::Text).is_err());
        assert!(
            validate_response(
                Bytes::from(vec![b'x'; MAX_RESPONSE_BYTES + 1]),
                ResponseFormat::Text
            )
            .is_err()
        );
    }
    #[test]
    fn speech_engines_receive_their_worker_contracts() {
        use crate::config::Provider;
        let wav = Bytes::from_static(b"RIFF\0\0\0\0WAVEdata");
        let request =
            |file: &Bytes, format, language: &str, prompt: &str, temperature| Transcription {
                model: "speech".into(),
                file: file.clone(),
                filename: "audio.wav".into(),
                response_format: format,
                temperature,
                language: language.into(),
                prompt: prompt.into(),
            };
        let code = |adapter, request| {
            worker_request(adapter, request)
                .err()
                .map(|error| error.code)
        };
        let opaque = Bytes::from_static(b"OggSaudio");
        for (adapter, path) in [
            (Provider::Parakeet, "/v1/audio/transcriptions"),
            (Provider::Asr4all, "/inference"),
            (Provider::Moonshine, "/inference"),
            (Provider::WhisperCpp, "/inference"),
        ] {
            let (selected, _) = worker_request(
                adapter,
                request(&wav, ResponseFormat::Json, "auto", "", 0.0),
            )
            .unwrap();
            assert_eq!(selected, path);
        }
        let rejected = Some("unsupported_transcription_option");
        for adapter in [Provider::Parakeet, Provider::Asr4all] {
            assert_eq!(
                code(
                    adapter,
                    request(&opaque, ResponseFormat::Json, "auto", "", 0.0)
                ),
                rejected
            );
        }
        assert_eq!(
            code(
                Provider::Moonshine,
                request(&opaque, ResponseFormat::Srt, "auto", "", 0.0)
            ),
            None
        );
        assert_eq!(
            code(
                Provider::Parakeet,
                request(&wav, ResponseFormat::Vtt, "auto", "", 0.0)
            ),
            rejected
        );
        assert_eq!(
            code(
                Provider::Parakeet,
                request(&wav, ResponseFormat::VerboseJson, "de", "x", 0.4)
            ),
            None
        );
        assert_eq!(
            code(
                Provider::Asr4all,
                request(&wav, ResponseFormat::VerboseJson, "auto", "", 0.0)
            ),
            rejected
        );
        assert_eq!(
            code(
                Provider::Asr4all,
                request(&wav, ResponseFormat::Text, "de", "", 0.0)
            ),
            rejected
        );
        assert_eq!(
            code(
                Provider::Asr4all,
                request(&wav, ResponseFormat::Text, "en", "bias", 0.0)
            ),
            rejected
        );
        assert_eq!(
            code(
                Provider::Asr4all,
                request(&wav, ResponseFormat::Text, "en", "", 0.5)
            ),
            rejected
        );
        assert_eq!(
            code(
                Provider::Asr4all,
                request(&wav, ResponseFormat::Text, "en", "", 0.0)
            ),
            None
        );
    }
}
