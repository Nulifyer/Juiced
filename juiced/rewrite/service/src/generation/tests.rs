use super::*;

fn audio(body: Value) -> Result<AudioRequest, ApiError> {
    AudioRequest::parse(&serde_json::to_vec(&body).unwrap())
}

fn mesh(body: Value) -> Result<MeshRequest, ApiError> {
    MeshRequest::parse(&serde_json::to_vec(&body).unwrap())
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

fn png64() -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(PNG)
}

#[test]
fn audio_requests_match_the_native_validation() {
    let missing = audio(json!({"prompt": "rain"})).unwrap_err();
    assert_eq!(missing.status, StatusCode::BAD_REQUEST);
    assert_eq!(missing.message, "Missing 'model' field in request");
    assert_eq!(
        audio(json!({"model": "m"})).unwrap_err().message,
        "Missing 'prompt' field in request"
    );
    for field in ["prompt", "lyrics", "vocal_language"] {
        let mut body = json!({"model": "m", "prompt": "rain"});
        body[field] = json!(3);
        assert_eq!(
            audio(body).unwrap_err().message,
            format!("'{field}' must be a string")
        );
    }
    let format =
        audio(json!({"model": "m", "prompt": "rain", "response_format": "mp3"})).unwrap_err();
    assert!(format.message.contains("supported: wav"));
    assert_eq!(
        audio(json!({"model": "m", "prompt": "rain", "temperature": 1}))
            .unwrap_err()
            .code,
        "unsupported_parameter"
    );
    for bad in [
        json!({"model": "m", "prompt": "rain", "duration": 0}),
        json!({"model": "m", "prompt": "rain", "duration": 601}),
        json!({"model": "m", "prompt": "rain", "steps": 0}),
        json!({"model": "m", "prompt": "rain", "seed": 1.5}),
        json!({"model": "m", "prompt": "rain", "vocal_language": "en us"}),
        json!({"model": "m", "prompt": "  "}),
    ] {
        assert_eq!(audio(bad).unwrap_err().status, StatusCode::BAD_REQUEST);
    }
}

#[test]
fn acestep_requests_choose_the_instrumental_or_vocal_pipeline() {
    for lyrics in [None, Some(""), Some("  [INSTRUMENTAL] ")] {
        let mut body =
            json!({"model": "m", "prompt": "lofi", "duration": 12, "steps": 8, "seed": 7});
        if let Some(lyrics) = lyrics {
            body["lyrics"] = json!(lyrics);
        }
        let request = audio(body).unwrap();
        assert!(!request.vocal());
        assert_eq!(
            request.acestep(),
            json!({"caption": "lofi", "synth_model": "", "output_format": "wav24", "duration": 12.0, "seed": 7, "inference_steps": 8, "lyrics": "[Instrumental]"})
        );
    }
    let vocal = audio(json!({"model": "m", "prompt": "folk", "lyrics": "[verse]\nla"})).unwrap();
    assert!(vocal.vocal());
    let synth = vocal.acestep();
    assert_eq!(synth["lm_mode"], "generate");
    assert_eq!(synth["vocal_language"], "en");
    assert_eq!(synth["lyrics"], "[verse]\nla");
}

#[test]
fn sound_effect_requests_map_native_aliases() {
    let request = audio(json!({"model": "m", "prompt": "glass", "cot": "shattering glass", "cfg": 4.5, "seconds": 5}))
        .unwrap();
    assert_eq!(
        request.thinksound(),
        json!({"caption": "glass", "description": "shattering glass", "duration": 5.0, "cfg": 4.5})
    );
    let plain = audio(json!({"model": "m", "prompt": "glass"})).unwrap();
    assert_eq!(plain.thinksound()["description"], "glass");
    let moss = audio(json!({"model": "m", "prompt": "door", "duration": 3, "cfg_scale": 2, "negative_prompt": "music", "seed": 1}))
        .unwrap();
    assert_eq!(
        moss.openmoss(),
        json!({"prompt": "door", "response_format": "wav", "seconds": 3.0, "cfg_scale": 2.0, "seed": 1, "negative_prompt": "music"})
    );
}

#[test]
fn mesh_requests_validate_before_any_model_load() {
    let request = mesh(json!({"model": "t", "image": format!("data:image/png;base64,{}", png64()), "resolution": 1024, "bg_removal": "birefnet", "uv": "box", "seed": 3}))
        .unwrap();
    assert_eq!(request.kind, ImageKind::Png);
    assert_eq!(request.image, PNG);
    assert_eq!(request.resolution, Some(1024));
    assert_eq!(
        mesh(json!({"model": "t", "image": png64(), "resolution": "1536"}))
            .unwrap()
            .resolution,
        Some(1536)
    );
    for (body, message) in [
        (
            json!({"image": png64()}),
            "Missing or non-string 'model' field in request",
        ),
        (
            json!({"model": "t"}),
            "Missing or non-string 'image' field in request (base64-encoded input image)",
        ),
        (
            json!({"model": "t", "image": "data:image/png;base64"}),
            "'image' data URL is missing its base64 payload",
        ),
        (
            json!({"model": "t", "image": "!!!"}),
            "'image' must be base64 data or a base64 data: URL",
        ),
        (
            json!({"model": "t", "image": "AAAAAAAAAAAAAAAA"}),
            "'image' is not a supported format (expected PNG, JPEG, BMP, or GIF)",
        ),
        (
            json!({"model": "t", "image": png64(), "response_format": "obj"}),
            "response_format 'obj' is not supported (supported: glb)",
        ),
        (
            json!({"model": "t", "image": png64(), "resolution": 768}),
            "'resolution' must be 512, 1024, or 1536",
        ),
        (
            json!({"model": "t", "image": png64(), "bg_removal": "magic"}),
            "'bg_removal' must be 'threshold' or 'birefnet'",
        ),
        (
            json!({"model": "t", "image": png64(), "uv": "sphere"}),
            "'uv' must be 'box' or 'xatlas'",
        ),
        (
            json!({"model": "t", "image": png64(), "seed": "1"}),
            "'seed' must be an integer",
        ),
    ] {
        assert_eq!(mesh(body).unwrap_err().message, message);
    }
}

#[test]
fn acestep_results_yield_their_first_audio_part() {
    let body = b"--ace-batch-boundary\r\nContent-Type: audio/wav\r\n\r\nRIFFdata\r\n--ace-batch-boundary\r\nContent-Type: application/octet-stream\r\nContent-Disposition: form-data; name=\"latent\"\r\n\r\n\x01\x02\r\n--ace-batch-boundary--\r\n";
    assert_eq!(acestep_audio(body), Some(&b"RIFFdata"[..]));
    assert_eq!(
        acestep_audio(b"--ace-batch-boundary\r\nContent-Type: application/json\r\n\r\n{}"),
        None
    );
}

#[test]
fn results_are_checked_by_container_magic() {
    let mut wav = b"RIFF\0\0\0\0WAVE".to_vec();
    wav.resize(44, 0);
    assert!(is_wav(&wav));
    assert!(!is_wav(b"RIFF\0\0\0\0AVI "));
    assert!(is_glb(b"glTF\x02\0\0\0\x0c\0\0\0"));
    assert!(!is_glb(b"glTF\x01\0\0\0\x0c\0\0\0"));
}

#[test]
fn openmoss_speech_accepts_descriptions_and_only_wav_or_pcm() {
    let speech = |body: Value| MossSpeech::parse(&serde_json::to_vec(&body).unwrap());
    let request = speech(json!({"model": "moss", "input": "hello", "voice": "A calm older narrator with a low voice", "instructions": "slowly"})).unwrap();
    assert_eq!(
        request.upstream(),
        json!({"input": "hello", "response_format": "wav", "voice": "A calm older narrator with a low voice", "instruction": "slowly"})
    );
    assert_eq!(
        speech(json!({"model": "moss", "input": "hi", "response_format": "pcm", "speed": 2}))
            .unwrap()
            .format,
        "pcm"
    );
    assert_eq!(
        speech(json!({"model": "moss", "input": "hi", "response_format": "mp3"}))
            .unwrap_err()
            .code,
        "unsupported_parameter"
    );
    assert_eq!(
        speech(json!({"model": "moss", "input": " "}))
            .unwrap_err()
            .code,
        "invalid_speech_request"
    );
}

#[test]
fn thenoise_requests_use_its_native_text2image_fields() {
    let request = crate::media::ImageRequest::parse(
        &serde_json::to_vec(&json!({"model": "anima", "prompt": "a cat", "size": "1024x768", "steps": 8, "cfg_scale": 1, "seed": 5, "negative_prompt": "blur"})).unwrap(),
    )
    .unwrap();
    assert_eq!(
        thenoise_body(&request),
        json!({"prompt": "a cat", "out": "json", "width": 1024, "height": 768, "negative_prompt": "blur", "steps": 8, "guidance_scale": 1.0, "seed": 5})
    );
}
