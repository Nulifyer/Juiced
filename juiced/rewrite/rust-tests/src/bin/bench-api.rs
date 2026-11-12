use bytes::Bytes;
use reqwest::{Client, Url};
use serde::{
    Deserialize, Deserializer,
    de::{IgnoredAny, MapAccess, Visitor},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    error::Error,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const MAXIMUM_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const MAXIMUM_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAXIMUM_EXPECTED_TOKENS: u64 = 1_000_000;

#[derive(Clone, Copy, Debug)]
struct ExpectedUsage {
    completion_tokens: Option<u64>,
    prompt_tokens: Option<u64>,
}

#[derive(Debug)]
struct Options {
    origin: Url,
    path: String,
    requests: usize,
    concurrency: usize,
    pid: Option<u32>,
    body_file: Option<PathBuf>,
    expected_usage: Option<ExpectedUsage>,
}

impl Options {
    fn parse() -> Result<Option<Self>, Box<dyn Error>> {
        Self::parse_arguments(std::env::args().skip(1))
    }

    fn parse_arguments(
        mut arguments: impl Iterator<Item = String>,
    ) -> Result<Option<Self>, Box<dyn Error>> {
        let mut origin = None;
        let mut path = "/v1/health".to_owned();
        let mut requests = 2000;
        let mut concurrency = 16;
        let mut pid = None;
        let mut body_file = None;
        let mut completion_tokens = None;
        let mut prompt_tokens = None;
        while let Some(argument) = arguments.next() {
            if argument == "--help" {
                println!(
                    "bench-api --url http://127.0.0.1:PORT [--path /v1/health] [--requests 2000] [--concurrency 16] [--pid PID] [--body-file JSON] [--expect-completion-tokens N] [--expect-prompt-tokens N]\nGET by default; --body-file selects POST. Usage expectations require --body-file and one completion choice. Completion tokens must be 1..=1000000; prompt tokens 0..=1000000. Keys use LEMONADE_API_KEY or LEMONADE_ADMIN_API_KEY and never appear in output."
                );
                return Ok(None);
            }
            let value = arguments
                .next()
                .ok_or("each benchmark option needs a value")?;
            match argument.as_str() {
                "--url" => origin = Some(Url::parse(&value)?),
                "--path" => path = value,
                "--requests" => requests = value.parse()?,
                "--concurrency" => concurrency = value.parse()?,
                "--pid" => pid = Some(value.parse()?),
                "--body-file" => body_file = Some(PathBuf::from(value)),
                "--expect-completion-tokens" => {
                    if completion_tokens.replace(value.parse::<u64>()?).is_some() {
                        return Err("choose --expect-completion-tokens once".into());
                    }
                }
                "--expect-prompt-tokens" => {
                    if prompt_tokens.replace(value.parse::<u64>()?).is_some() {
                        return Err("choose --expect-prompt-tokens once".into());
                    }
                }
                _ => return Err(format!("unknown benchmark option: {argument}").into()),
            }
        }
        let origin = origin.ok_or("--url is required")?;
        if !matches!(origin.scheme(), "http" | "https")
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
            || origin.path() != "/"
        {
            return Err(
                "--url must be an HTTP(S) origin without credentials, path, query or fragment"
                    .into(),
            );
        }
        if !path.starts_with('/') || path.starts_with("//") || path.contains(['?', '#']) {
            return Err(
                "--path must be an absolute endpoint path without query or fragment".into(),
            );
        }
        if !(1..=1_000_000).contains(&requests) || !(1..=1024).contains(&concurrency) {
            return Err("requests must be 1..=1000000 and concurrency 1..=1024".into());
        }
        if completion_tokens.is_some_and(|count| !(1..=MAXIMUM_EXPECTED_TOKENS).contains(&count))
            || prompt_tokens.is_some_and(|count| count > MAXIMUM_EXPECTED_TOKENS)
        {
            return Err(
                "completion tokens must be 1..=1000000 and prompt tokens 0..=1000000".into(),
            );
        }
        let expected_usage = if completion_tokens.is_some() || prompt_tokens.is_some() {
            if body_file.is_none() {
                return Err("token expectations require --body-file".into());
            }
            Some(ExpectedUsage {
                completion_tokens,
                prompt_tokens,
            })
        } else {
            None
        };
        Ok(Some(Self {
            origin,
            path,
            requests,
            concurrency: concurrency.min(requests),
            pid,
            body_file,
            expected_usage,
        }))
    }
}

struct CheckedString<const NONEMPTY: bool>;
impl<'de, const NONEMPTY: bool> Deserialize<'de> for CheckedString<NONEMPTY> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct StringVisitor<const NONEMPTY: bool>;
        impl<const NONEMPTY: bool> Visitor<'_> for StringVisitor<NONEMPTY> {
            type Value = CheckedString<NONEMPTY>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(if NONEMPTY {
                    "a nonempty string"
                } else {
                    "a string"
                })
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                if NONEMPTY && value.is_empty() {
                    return Err(E::custom("a nonempty string is required"));
                }
                Ok(CheckedString)
            }
        }
        deserializer.deserialize_str(StringVisitor::<NONEMPTY>)
    }
}
#[derive(Deserialize)]
struct ChoiceFields {
    #[serde(rename = "text")]
    _text: CheckedString<false>,
    index: u64,
    #[serde(rename = "finish_reason")]
    _finish_reason: CheckedString<true>,
}
struct CompletionChoice {
    index: u64,
}
impl<'de> Deserialize<'de> for CompletionChoice {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ChoiceVisitor;
        impl<'de> Visitor<'de> for ChoiceVisitor {
            type Value = CompletionChoice;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("one completion choice object")
            }
            fn visit_map<M: MapAccess<'de>>(self, fields: M) -> Result<Self::Value, M::Error> {
                let fields = ChoiceFields::deserialize(
                    serde::de::value::MapAccessDeserializer::new(fields),
                )?;
                Ok(CompletionChoice {
                    index: fields.index,
                })
            }
        }
        deserializer.deserialize_map(ChoiceVisitor)
    }
}
#[derive(Deserialize)]
struct Usage {
    prompt_tokens: u64,
    completion_tokens: u64,
}
#[derive(Default)]
struct ErrorPresence(bool);
impl<'de> Deserialize<'de> for ErrorPresence {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        IgnoredAny::deserialize(deserializer)?;
        Ok(Self(true))
    }
}
#[derive(Deserialize)]
struct CompletionMetadata {
    choices: Option<[CompletionChoice; 1]>,
    usage: Option<Usage>,
    #[serde(default)]
    error: ErrorPresence,
}
fn validate_completion(bytes: &[u8], expected: ExpectedUsage) -> Result<(), &'static str> {
    let response: CompletionMetadata =
        serde_json::from_slice(bytes).map_err(|_| "invalid_completion_json")?;
    if response.error.0 {
        return Err("upstream_error");
    }
    let [choice] = response.choices.ok_or("missing_choices")?;
    if choice.index != 0 {
        return Err("choice_index_mismatch");
    }
    let usage = response.usage.ok_or("missing_usage")?;
    if expected
        .completion_tokens
        .is_some_and(|count| usage.completion_tokens != count)
    {
        return Err("completion_tokens_mismatch");
    }
    if expected
        .prompt_tokens
        .is_some_and(|count| usage.prompt_tokens != count)
    {
        return Err("prompt_tokens_mismatch");
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Validation {
    NotRequested,
    Passed,
    Failed(&'static str),
}
#[derive(Debug)]
struct Exchange {
    status: u16,
    bytes: u64,
    body_latency_ns: Option<u64>,
    validation: Validation,
}
#[derive(Default)]
struct Samples {
    latency_ns: Vec<u64>,
    successful: usize,
    errors: usize,
    bytes: u64,
    statuses: BTreeMap<u16, usize>,
    transport_failures: BTreeMap<&'static str, usize>,
    validation_checked: usize,
    validation_passed: usize,
    validation_failures: BTreeMap<&'static str, usize>,
    oversized_response_bodies: usize,
}
impl Samples {
    fn record(&mut self, response: Result<Exchange, reqwest::Error>, retain_latency: bool) -> bool {
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                self.errors += 1;
                *self
                    .transport_failures
                    .entry(if error.is_timeout() {
                        "timeout"
                    } else {
                        "transport"
                    })
                    .or_default() += 1;
                return false;
            }
        };
        *self.statuses.entry(response.status).or_default() += 1;
        self.bytes += response.bytes;
        match response.validation {
            Validation::NotRequested => {}
            Validation::Passed => {
                self.validation_checked += 1;
                self.validation_passed += 1;
            }
            Validation::Failed(reason) => {
                self.validation_checked += 1;
                *self.validation_failures.entry(reason).or_default() += 1;
                self.oversized_response_bodies += usize::from(reason == "response_too_large");
            }
        }
        let successful = (200..300).contains(&response.status)
            && !matches!(response.validation, Validation::Failed(_));
        if successful {
            self.successful += 1;
            if retain_latency {
                self.latency_ns.push(
                    response
                        .body_latency_ns
                        .expect("successful response body completed"),
                );
            }
        } else {
            self.errors += 1;
        }
        successful
    }
    fn merge(&mut self, other: Self) {
        self.latency_ns.extend(other.latency_ns);
        self.successful += other.successful;
        self.errors += other.errors;
        self.bytes += other.bytes;
        self.validation_checked += other.validation_checked;
        self.validation_passed += other.validation_passed;
        self.oversized_response_bodies += other.oversized_response_bodies;
        for (status, count) in other.statuses {
            *self.statuses.entry(status).or_default() += count;
        }
        for (reason, count) in other.transport_failures {
            *self.transport_failures.entry(reason).or_default() += count;
        }
        for (reason, count) in other.validation_failures {
            *self.validation_failures.entry(reason).or_default() += count;
        }
    }
}

async fn exchange(
    client: &Client,
    url: &Url,
    body: Option<&Bytes>,
    expected: Option<ExpectedUsage>,
) -> Result<Exchange, reqwest::Error> {
    let started = Instant::now();
    let request = match body {
        Some(body) => client
            .post(url.clone())
            .header("content-type", "application/json")
            .body(body.clone()),
        None => client.get(url.clone()),
    };
    let mut response = request.send().await?;
    let status = response.status().as_u16();
    let mut bytes = 0u64;
    let mut buffered = (expected.is_some() && status == 200).then(Vec::new);
    while let Some(chunk) = response.chunk().await? {
        bytes = bytes.saturating_add(chunk.len() as u64);
        if expected.is_some() && bytes > MAXIMUM_RESPONSE_BYTES as u64 {
            return Ok(Exchange {
                status,
                bytes,
                body_latency_ns: None,
                validation: Validation::Failed("response_too_large"),
            });
        }
        if let Some(buffer) = &mut buffered {
            buffer.extend_from_slice(&chunk);
        }
    }
    let body_latency_ns = Some(started.elapsed().as_nanos().min(u64::MAX as u128) as u64);
    let validation = match expected {
        None => Validation::NotRequested,
        Some(_) if status != 200 => Validation::Failed("unexpected_http_status"),
        Some(expected) => {
            match validate_completion(buffered.as_deref().unwrap_or_default(), expected) {
                Ok(()) => Validation::Passed,
                Err(reason) => Validation::Failed(reason),
            }
        }
    };
    Ok(Exchange {
        status,
        bytes,
        body_latency_ns,
        validation,
    })
}

fn percentile_us(samples: &[u64], percentage: usize) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let rank = (samples.len() * percentage)
        .div_ceil(100)
        .clamp(1, samples.len());
    Some(samples[rank - 1] as f64 / 1000.0)
}

struct ProcessSample {
    ticks: u64,
    start_ticks: u64,
    rss_bytes: u64,
    ticks_per_second: u64,
}

#[cfg(target_os = "linux")]
fn process_sample(pid: u32) -> Option<ProcessSample> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let values: Vec<&str> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
    let user: u64 = values.get(11)?.parse().ok()?;
    let system: u64 = values.get(12)?.parse().ok()?;
    let start_ticks = values.get(19)?.parse().ok()?;
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let rss_bytes = status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))?
        .split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()?
        .checked_mul(1024)?;
    let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks_per_second <= 0 {
        return None;
    }
    Some(ProcessSample {
        ticks: user.checked_add(system)?,
        start_ticks,
        rss_bytes,
        ticks_per_second: ticks_per_second as u64,
    })
}

#[cfg(not(target_os = "linux"))]
fn process_sample(_pid: u32) -> Option<ProcessSample> {
    None
}

fn workload(options: &Options, body: Option<&Bytes>, authenticated: bool) -> serde_json::Value {
    json!({"origin":options.origin.origin().ascii_serialization(),"path":options.path,
        "method":if body.is_some(){"POST"}else{"GET"},"requests":options.requests,"concurrency":options.concurrency,
        "warmup_requests":options.concurrency*8,"maximum_measurement_seconds":120,
        "body_sha256":body.map(|body|format!("{:x}",Sha256::digest(body))),"authenticated":authenticated})
}
fn validation_counts(samples: &Samples) -> serde_json::Value {
    json!({"checked":samples.validation_checked,"passed":samples.validation_passed,"failures":samples.validation_failures,
        "oversized_response_bodies":samples.oversized_response_bodies})
}
fn validation_receipt(
    expected: Option<ExpectedUsage>,
    warmup: &Samples,
    measured: Option<&Samples>,
) -> serde_json::Value {
    json!({"enabled":expected.is_some(),"expected_completion_tokens":expected.and_then(|expected|expected.completion_tokens),
        "expected_prompt_tokens":expected.and_then(|expected|expected.prompt_tokens),
        "maximum_response_bytes":expected.map(|_|MAXIMUM_RESPONSE_BYTES),
        "scope":"HTTP 200; exactly one choice object with string text, index zero and nonempty string finish_reason; unsigned prompt/completion usage counts match expectations. Text content is discarded without scoring.",
        "warmup":validation_counts(warmup),"measured":measured.map(validation_counts)})
}
fn warmup_counts(samples: &Samples) -> serde_json::Value {
    json!({"attempted":samples.successful+samples.errors,"successful":samples.successful,"errors":samples.errors,
        "statuses":samples.statuses,"transport_failures":samples.transport_failures})
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let Some(options) = Options::parse()? else {
        return Ok(());
    };
    let body = if let Some(path) = &options.body_file {
        let file = std::fs::File::open(path)?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(file, MAXIMUM_REQUEST_BYTES as u64 + 1),
            &mut bytes,
        )?;
        if bytes.len() > MAXIMUM_REQUEST_BYTES {
            return Err("benchmark JSON body exceeds 2 MiB".into());
        }
        let _: IgnoredAny = serde_json::from_slice(&bytes)?;
        Some(Bytes::from(bytes))
    } else {
        None
    };
    let url = options.origin.join(&options.path)?;
    let mut headers = reqwest::header::HeaderMap::new();
    let key = std::env::var("LEMONADE_ADMIN_API_KEY")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("LEMONADE_API_KEY")
                .ok()
                .filter(|value| !value.is_empty())
        });
    if let Some(key) = &key {
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {key}").parse()?,
        );
    }
    let client = Client::builder()
        .default_headers(headers)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(options.concurrency)
        .build()?;
    let mut warmups = tokio::task::JoinSet::new();
    for _ in 0..options.concurrency {
        let client = client.clone();
        let url = url.clone();
        let body = body.clone();
        let expected = options.expected_usage;
        warmups.spawn(async move {
            let mut samples = Samples::default();
            for _ in 0..8 {
                if !samples.record(
                    exchange(&client, &url, body.as_ref(), expected).await,
                    false,
                ) {
                    break;
                }
            }
            samples
        });
    }
    let mut warmup = Samples::default();
    let mut warmup_error = None;
    while let Some(result) = warmups.join_next().await {
        match result {
            Ok(samples) => warmup.merge(samples),
            Err(_) => {
                warmup_error = Some("warmup_worker_failed");
                warmups.abort_all();
                break;
            }
        }
    }
    if warmup.errors > 0 || warmup_error.is_some() {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"schema_version":1,"phase":"warmup","passing":false,
            "measurement_complete":false,"terminal_error":warmup_error.unwrap_or("warmup_requests_failed"),
            "workload":workload(&options,body.as_ref(),key.is_some()),"warmup":warmup_counts(&warmup),
            "validation":validation_receipt(options.expected_usage,&warmup,None)})
            )?
        );
        return Err("benchmark warmup failed; partial JSON receipt contains status, transport and validation counts".into());
    }
    let before = options.pid.and_then(process_sample);
    let peak_rss = Arc::new(AtomicU64::new(
        before.as_ref().map_or(0, |sample| sample.rss_bytes),
    ));
    let sampling = Arc::new(AtomicBool::new(true));
    let sampler = options.pid.map(|pid| {
        let sampling = sampling.clone();
        let peak_rss = peak_rss.clone();
        tokio::spawn(async move {
            while sampling.load(Ordering::Relaxed) {
                if let Some(sample) = process_sample(pid) {
                    peak_rss.fetch_max(sample.rss_bytes, Ordering::Relaxed);
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    });
    let barrier = Arc::new(tokio::sync::Barrier::new(options.concurrency + 1));
    let mut workers = tokio::task::JoinSet::new();
    for index in 0..options.concurrency {
        let client = client.clone();
        let url = url.clone();
        let body = body.clone();
        let barrier = barrier.clone();
        let count = options.requests;
        let step = options.concurrency;
        let expected = options.expected_usage;
        workers.spawn(async move {
            let mut samples = Samples {
                latency_ns: Vec::with_capacity(count.div_ceil(step)),
                ..Samples::default()
            };
            barrier.wait().await;
            for _ in (index..count).step_by(step) {
                samples.record(exchange(&client, &url, body.as_ref(), expected).await, true);
            }
            samples
        });
    }
    let started = Instant::now();
    barrier.wait().await;
    let mut total = Samples {
        latency_ns: Vec::with_capacity(options.requests),
        ..Samples::default()
    };
    let deadline = tokio::time::sleep(Duration::from_secs(120));
    tokio::pin!(deadline);
    let mut terminal_error = None;
    loop {
        let result = tokio::select! {
            result=workers.join_next()=>result,
            _=&mut deadline=>{terminal_error=Some("measurement_deadline");workers.abort_all();break;}
        };
        match result {
            Some(Ok(samples)) => total.merge(samples),
            Some(Err(_)) => {
                terminal_error = Some("measurement_worker_failed");
                workers.abort_all();
                break;
            }
            None => break,
        }
    }
    let elapsed = started.elapsed();
    sampling.store(false, Ordering::Relaxed);
    if let Some(sampler) = sampler {
        sampler.await?;
    }
    let after = options.pid.and_then(process_sample);
    let process = match (before, after) {
        (Some(before), Some(after)) if before.start_ticks == after.start_ticks => json!({
            "pid":options.pid,"cpu_time_ms":after.ticks.saturating_sub(before.ticks) as f64*1000.0/before.ticks_per_second as f64,
            "cpu_resolution_ms":1000.0/before.ticks_per_second as f64,"rss_before_bytes":before.rss_bytes,"rss_after_bytes":after.rss_bytes,
            "peak_sampled_rss_bytes":peak_rss.load(Ordering::Relaxed),"rss_sampling_interval_ms":5,
            "scope":"Linux process only; child-worker CPU/RSS excluded. RSS sampling may miss shorter peaks."}),
        _ => serde_json::Value::Null,
    };
    total.latency_ns.sort_unstable();
    let recorded = total.successful + total.errors;
    let complete = terminal_error.is_none() && recorded == options.requests;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema_version":1,"phase":"measurement","measurement_complete":complete,"passing":complete && total.errors==0,
            "terminal_error":terminal_error,"recorded_requests":recorded,"unrecorded_requests":options.requests.saturating_sub(recorded),
            "scope":"Warmed HTTP request and response-body completion. No inference quality or release qualification claim.",
            "workload":workload(&options,body.as_ref(),key.is_some()),"warmup":warmup_counts(&warmup),
            "validation":validation_receipt(options.expected_usage,&warmup,Some(&total)),
            "successful":total.successful,"errors":total.errors,"response_bytes":total.bytes,
            "statuses":total.statuses,"transport_failures":total.transport_failures,
            "elapsed_ms":elapsed.as_secs_f64()*1000.0,"successful_requests_per_second":total.successful as f64/elapsed.as_secs_f64(),
            "latency_scope":"Successful response-body completion before optional JSON usage validation; nearest-rank percentiles.",
            "throughput_scope":"Client wall time includes response handling, optional validation, scheduling and sample collection; warmup and receipt serialization excluded.",
            "partial_result_scope":"Unrecorded requests include unfinished worker samples; their responses are not represented in latency, status or validation counts.",
            "latency_us":{"p50":percentile_us(&total.latency_ns,50),"p95":percentile_us(&total.latency_ns,95),"p99":percentile_us(&total.latency_ns,99)},"process":process
        }))?
    );
    if total.errors > 0 || !complete {
        return Err("benchmark included failed or unrecorded requests; JSON receipt is not a passing performance gate".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn expected() -> ExpectedUsage {
        ExpectedUsage {
            completion_tokens: Some(1),
            prompt_tokens: Some(10),
        }
    }
    fn options(extra: &[&str]) -> Result<Option<Options>, Box<dyn Error>> {
        Options::parse_arguments(
            ["--url", "http://127.0.0.1:1"]
                .into_iter()
                .chain(extra.iter().copied())
                .map(str::to_owned),
        )
    }
    fn valid() -> &'static [u8] {
        br#"{"choices":[{"text":"private-output-do-not-retain","index":0,"finish_reason":"length"}],"usage":{"prompt_tokens":10,"completion_tokens":1,"total_tokens":11}}"#
    }
    async fn reply(body: Vec<u8>) -> (Url, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!(
            "http://{}/v1/completions",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") && headers.len() < 8192 {
                let mut byte = [0];
                if socket.read_exact(&mut byte).await.is_err() {
                    return;
                }
                headers.push(byte[0]);
            }
            let length = String::from_utf8_lossy(&headers)
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            assert!(length <= 1024);
            let mut request_body = vec![0; length];
            if socket.read_exact(&mut request_body).await.is_err() {
                return;
            }
            if socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .await
                .is_err()
            {
                return;
            }
            for chunk in body.chunks(4096) {
                if socket
                    .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                    .await
                    .is_err()
                    || socket.write_all(chunk).await.is_err()
                    || socket.write_all(b"\r\n").await.is_err()
                {
                    return;
                }
            }
            let _ = socket.write_all(b"0\r\n\r\n").await;
            let _ = socket.shutdown().await;
        });
        (url, task)
    }
    fn client() -> Client {
        Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap()
    }

    #[test]
    fn percentiles_use_successful_samples_with_explicit_empty_state() {
        assert_eq!(percentile_us(&[], 50), None);
        assert_eq!(percentile_us(&[1000, 2000, 3000, 4000], 50), Some(2.0));
        assert_eq!(percentile_us(&[1000, 2000, 3000, 4000], 95), Some(4.0));
        assert_eq!(percentile_us(&[1000], 99), Some(1.0));
    }

    #[test]
    fn usage_validation_requires_one_choice_unsigned_counts_and_no_error_field() {
        assert_eq!(validate_completion(valid(), expected()), Ok(()));
        assert_eq!(validate_completion(br#"{"choices":[{"text":"\n\u03bb","index":0,"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#,expected()),Ok(()));
        for invalid in [
            br#"{"error":{"message":"private-output-do-not-retain"}}"#.as_slice(),
            br#"{"error":null,"choices":[{"text":"","index":0,"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#,
            br#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#,
            br#"{"choices":[{},{}],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#,
            br#"{"choices":[null],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#,
            br#"{"choices":[[]],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#,
            br#"{"choices":[{"text":"","index":0,"finish_reason":"stop"}]}"#,
            br#"{"usage":{"prompt_tokens":10,"completion_tokens":1}}"#,
            br#"{"choices":[{"text":"","index":0,"finish_reason":"stop"}],"usage":{"prompt_tokens":-1,"completion_tokens":1}}"#,
            br#"{"choices":[{"text":"","index":0,"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":1.0}}"#,
            br#"{"choices":[{"text":"","index":0,"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":"1"}}"#,
            br#"{"choices":[{"text":"","index":0,"finish_reason":"stop"}],"usage":{"prompt_tokens":10}}"#,
            br#"{"choices":[{}],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#,
            br#"{"choices":[{"index":0,"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#,
            br#"{"choices":[{"text":42,"index":0,"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#,
            br#"{"choices":[{"text":"","finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#,
            br#"{"choices":[{"text":"","index":1,"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#,
            br#"{"choices":[{"text":"","index":"0","finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#,
            br#"{"choices":[{"text":"","index":0}],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#,
            br#"{"choices":[{"text":"","index":0,"finish_reason":""}],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#,
            br#"{"choices":[{"text":"","index":0,"finish_reason":null}],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#,
            b"{",
        ] {
            assert!(validate_completion(invalid, expected()).is_err());
        }
        assert_eq!(
            validate_completion(
                br#"{"choices":[{"text":"","index":0,"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2}}"#,
                expected()
            ),
            Err("completion_tokens_mismatch")
        );
        assert_eq!(
            validate_completion(
                br#"{"choices":[{"text":"","index":0,"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":1}}"#,
                expected()
            ),
            Err("prompt_tokens_mismatch")
        );
    }

    #[test]
    fn expectation_cli_requires_body_and_bounded_explicit_token_counts() {
        assert!(options(&[]).unwrap().unwrap().expected_usage.is_none());
        let configured = options(&[
            "--body-file",
            "input.json",
            "--expect-completion-tokens",
            "1",
            "--expect-prompt-tokens",
            "0",
        ])
        .unwrap()
        .unwrap();
        let expected = configured.expected_usage.unwrap();
        assert_eq!(expected.completion_tokens, Some(1));
        assert_eq!(expected.prompt_tokens, Some(0));
        for invalid in [
            vec!["--expect-completion-tokens", "1"],
            vec!["--expect-prompt-tokens", "0"],
            vec![
                "--body-file",
                "input.json",
                "--expect-completion-tokens",
                "0",
            ],
            vec![
                "--body-file",
                "input.json",
                "--expect-completion-tokens",
                "1000001",
            ],
            vec![
                "--body-file",
                "input.json",
                "--expect-prompt-tokens",
                "1000001",
            ],
            vec![
                "--body-file",
                "input.json",
                "--expect-completion-tokens",
                "-1",
            ],
            vec![
                "--body-file",
                "input.json",
                "--expect-prompt-tokens",
                "18446744073709551616",
            ],
            vec![
                "--body-file",
                "input.json",
                "--expect-completion-tokens",
                "1",
                "--expect-completion-tokens",
                "1",
            ],
            vec![
                "--body-file",
                "input.json",
                "--expect-prompt-tokens",
                "1",
                "--expect-prompt-tokens",
                "2",
            ],
            vec!["--expect-completion-tokens"],
            vec!["--requests", "0"],
            vec!["--concurrency", "1025"],
        ] {
            assert!(options(&invalid).is_err());
        }
    }

    #[tokio::test]
    async fn malformed_error_and_wrong_usage_http_200_responses_are_failed_samples() {
        let client = client();
        for (body, reason) in [
            (
                br#"{"error":"private-output-do-not-retain"}"#.as_slice(),
                "upstream_error",
            ),
            (b"{".as_slice(), "invalid_completion_json"),
            (
                br#"{"choices":[{"text":"","index":0,"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2}}"#
                    .as_slice(),
                "completion_tokens_mismatch",
            ),
        ] {
            let (url, server) = reply(body.to_vec()).await;
            let exchange = exchange(
                &client,
                &url,
                Some(&Bytes::from_static(b"{}")),
                Some(expected()),
            )
            .await
            .unwrap();
            assert_eq!(exchange.status, 200);
            assert_eq!(exchange.validation, Validation::Failed(reason));
            assert!(exchange.body_latency_ns.is_some());
            let mut samples = Samples::default();
            assert!(!samples.record(Ok(exchange), true));
            assert_eq!((samples.successful, samples.errors), (0, 1));
            assert_eq!(samples.statuses.get(&200), Some(&1));
            assert_eq!(samples.validation_checked, 1);
            assert_eq!(samples.validation_passed, 0);
            assert_eq!(samples.validation_failures.get(reason), Some(&1));
            assert!(samples.latency_ns.is_empty());
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn oversized_chunked_json_is_rejected_while_default_get_mode_streams_without_validation()
    {
        let client = client();
        let (url, server) = reply(vec![b' '; MAXIMUM_RESPONSE_BYTES + 1]).await;
        let rejected = exchange(
            &client,
            &url,
            Some(&Bytes::from_static(b"{}")),
            Some(expected()),
        )
        .await
        .unwrap();
        assert_eq!(
            rejected.validation,
            Validation::Failed("response_too_large")
        );
        assert!(rejected.body_latency_ns.is_none());
        let mut samples = Samples::default();
        assert!(!samples.record(Ok(rejected), true));
        assert_eq!(samples.oversized_response_bodies, 1);
        assert!(samples.latency_ns.is_empty());
        server.await.unwrap();
        let (url, server) = reply(vec![b' '; MAXIMUM_RESPONSE_BYTES + 1]).await;
        let streamed = exchange(&client, &url, None, None).await.unwrap();
        assert_eq!(streamed.bytes, MAXIMUM_RESPONSE_BYTES as u64 + 1);
        assert_eq!(streamed.validation, Validation::NotRequested);
        let mut samples = Samples::default();
        assert!(samples.record(Ok(streamed), true));
        assert_eq!(samples.validation_checked, 0);
        assert_eq!(samples.latency_ns.len(), 1);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn valid_response_receipt_separates_warmup_and_measurement_without_retaining_output() {
        let client = client();
        let mut warmup = Samples::default();
        let mut measured = Samples::default();
        for samples in [&mut warmup, &mut measured] {
            let (url, server) = reply(valid().to_vec()).await;
            let response = exchange(
                &client,
                &url,
                Some(&Bytes::from_static(b"{}")),
                Some(expected()),
            )
            .await
            .unwrap();
            assert_eq!(response.validation, Validation::Passed);
            assert!(samples.record(Ok(response), true));
            server.await.unwrap();
        }
        let receipt = validation_receipt(Some(expected()), &warmup, Some(&measured));
        assert_eq!(receipt["warmup"]["checked"], 1);
        assert_eq!(receipt["warmup"]["passed"], 1);
        assert_eq!(receipt["measured"]["checked"], 1);
        assert_eq!(receipt["measured"]["passed"], 1);
        assert_eq!(receipt["maximum_response_bytes"], MAXIMUM_RESPONSE_BYTES);
        assert!(!receipt.to_string().contains("private-output-do-not-retain"));
    }
}
