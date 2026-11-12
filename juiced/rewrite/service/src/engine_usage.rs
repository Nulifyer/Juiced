use serde::Serialize;
use serde_json::Value;

pub const TAIL_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Observation {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub decode_tokens_per_second: Option<f64>,
    pub prompt_tokens_per_second: Option<f64>,
}

impl Observation {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Default)]
pub struct Tail {
    bytes: Vec<u8>,
}

impl Tail {
    pub fn push(&mut self, chunk: &[u8]) {
        if chunk.len() >= TAIL_BYTES {
            self.bytes.clear();
            self.bytes
                .extend_from_slice(&chunk[chunk.len() - TAIL_BYTES..]);
            return;
        }
        let excess = (self.bytes.len() + chunk.len()).saturating_sub(TAIL_BYTES);
        self.bytes.drain(..excess);
        self.bytes.extend_from_slice(chunk);
    }

    pub fn observe(&self) -> Observation {
        observe(&self.bytes)
    }
}

fn object_after_key<'a>(text: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    let mut end = text.len();
    while let Some(position) = find_last(&text[..end], key) {
        end = position;
        let mut index = position + key.len();
        while text.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        if text.get(index) != Some(&b':') {
            continue;
        }
        index += 1;
        while text.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        if text.get(index) != Some(&b'{') {
            continue;
        }
        if let Some(object) = balanced_object(&text[index..]) {
            return Some(object);
        }
    }
    None
}

fn find_last(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

fn balanced_object(text: &[u8]) -> Option<&[u8]> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, &byte) in text.iter().enumerate() {
        if in_string {
            match (escaped, byte) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&text[..=index]);
                }
            }
            _ => {}
        }
    }
    None
}

fn count(object: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| object.get(*key)?.as_u64())
}

fn rate(object: &Value, key: &str) -> Option<f64> {
    object
        .get(key)?
        .as_f64()
        .filter(|value| value.is_finite() && *value > 0.0)
}

pub fn observe(tail: &[u8]) -> Observation {
    let parse = |key: &[u8]| -> Option<Value> {
        serde_json::from_slice::<Value>(object_after_key(tail, key)?)
            .ok()
            .filter(Value::is_object)
    };
    let timings = parse(b"\"timings\"");
    let usage = parse(b"\"usage\"");
    let timing_input = timings.as_ref().and_then(|timings| {
        let prompt = count(timings, &["prompt_n"])?;
        prompt.checked_add(count(timings, &["cache_n"]).unwrap_or(0))
    });
    Observation {
        input_tokens: usage
            .as_ref()
            .and_then(|usage| count(usage, &["prompt_tokens", "input_tokens"]))
            .or(timing_input),
        output_tokens: usage
            .as_ref()
            .and_then(|usage| count(usage, &["completion_tokens", "output_tokens"]))
            .or_else(|| {
                timings
                    .as_ref()
                    .and_then(|timings| count(timings, &["predicted_n"]))
            }),
        decode_tokens_per_second: timings
            .as_ref()
            .and_then(|timings| rate(timings, "predicted_per_second")),
        prompt_tokens_per_second: timings
            .as_ref()
            .and_then(|timings| rate(timings, "prompt_per_second")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn non_streaming_llama_reply_reports_usage_and_decode_rate() {
        let reply = json!({
            "choices":[{"message":{"content":"say \"timings\":{\"predicted_per_second\":1}"}}],
            "usage":{"completion_tokens":12,"prompt_tokens":30,"total_tokens":42,"prompt_tokens_details":{"cached_tokens":4}},
            "timings":{"cache_n":4,"prompt_n":26,"prompt_per_second":812.5,"predicted_n":12,"predicted_per_second":95.25}
        });
        let observed = observe(reply.to_string().as_bytes());
        assert_eq!(
            observed,
            Observation {
                input_tokens: Some(30),
                output_tokens: Some(12),
                decode_tokens_per_second: Some(95.25),
                prompt_tokens_per_second: Some(812.5),
            }
        );
    }

    #[test]
    fn streaming_final_chunk_supplies_timings_without_usage() {
        let mut tail = Tail::default();
        tail.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"usage\"}}]}\n\n");
        tail.push(b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"timings\":{\"prompt_n\":5,\"cache_n\":2,\"predicted_n\":9,\"predicted_per_second\":41.5,\"prompt_per_second\":0}}\n\ndata: [DONE]\n\n");
        assert_eq!(
            tail.observe(),
            Observation {
                input_tokens: Some(7),
                output_tokens: Some(9),
                decode_tokens_per_second: Some(41.5),
                prompt_tokens_per_second: None,
            }
        );
    }

    #[test]
    fn responses_usage_names_are_recognized_and_absent_data_is_empty() {
        let reply = json!({"output":[],"usage":{"input_tokens":3,"output_tokens":4}});
        let observed = observe(reply.to_string().as_bytes());
        assert_eq!(observed.input_tokens, Some(3));
        assert_eq!(observed.output_tokens, Some(4));
        assert_eq!(observed.decode_tokens_per_second, None);
        assert!(observe(b"{\"choices\":[]}").is_empty());
        assert!(observe(b"{\"timings\":{\"predicted_per_second\":").is_empty());
        assert!(observe(b"{\"timings\":\"none\"}").is_empty());
    }

    #[test]
    fn tail_retains_only_the_final_bytes() {
        let mut tail = Tail::default();
        tail.push(&vec![b'a'; TAIL_BYTES * 2]);
        assert_eq!(tail.bytes.len(), TAIL_BYTES);
        tail.push(b"{\"timings\":{\"predicted_per_second\":3.5}}");
        assert_eq!(tail.bytes.len(), TAIL_BYTES);
        assert_eq!(tail.observe().decode_tokens_per_second, Some(3.5));
        for _ in 0..100 {
            tail.push(&[b'b'; 1000]);
        }
        assert_eq!(tail.bytes.len(), TAIL_BYTES);
        assert!(tail.observe().is_empty());
    }
}
