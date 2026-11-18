#![cfg(target_os = "linux")]

mod support;

use futures_util::StreamExt;
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;
use support::Server;

const MODEL: &str = "matrix";
const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;

fn variable(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn lines() -> usize {
    variable("JUICED_TEST_MATRIX_LINES").map_or(60, |value| value.parse().unwrap())
}

/// Text that no two seeds share, so a prompt reuses only what it repeats on purpose.
fn document(seed: u64, lines: usize) -> String {
    const WORDS: [&str; 16] = [
        "kitchen",
        "porch",
        "garage",
        "hallway",
        "office",
        "bedroom",
        "thermostat",
        "sensor",
        "lamp",
        "fan",
        "door",
        "switch",
        "blind",
        "valve",
        "meter",
        "relay",
    ];
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    (0..lines)
        .map(|index| {
            format!(
                "Record {seed}-{index}: the {} {} in area {} reports {} and feeds the {}.",
                WORDS[(next() % 16) as usize],
                WORDS[(next() % 16) as usize],
                next() % 97,
                next() % 100_000,
                WORDS[(next() % 16) as usize],
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn system(text: &str) -> Value {
    json!({"role":"system","content":text})
}

fn user(text: &str) -> Value {
    json!({"role":"user","content":text})
}

struct Lab {
    server: Server,
    client: reqwest::Client,
    seen: u64,
    filler: u32,
    rows: Vec<Value>,
}

impl Lab {
    async fn start(slots: u32, session_cache: Value) -> Self {
        let engine = PathBuf::from(
            variable("JUICED_TEST_LLAMA_SERVER").expect("set JUICED_TEST_LLAMA_SERVER"),
        );
        let model = PathBuf::from(
            variable("JUICED_TEST_PREFIX_GGUF").expect("set JUICED_TEST_PREFIX_GGUF"),
        );
        let context = variable("JUICED_TEST_MATRIX_CONTEXT").unwrap_or_else(|| "12288".into());
        let threads = variable("JUICED_TEST_THREADS").unwrap_or_else(|| "8".into());
        let mut args = vec![
            "--host",
            "127.0.0.1",
            "--port",
            "{port}",
            "--model",
            "{model}",
            "--slots",
            "--n-gpu-layers",
            "0",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        args.extend(["--ctx-size".to_owned(), context]);
        args.extend(["--threads".to_owned(), threads]);
        args.extend(["--parallel".to_owned(), slots.to_string()]);
        // The engine's own RAM prompt cache also restores earlier prompts. It is off by default
        // here so the rows show what Juiced does for states too large for it.
        args.extend([
            "--cache-ram".to_owned(),
            variable("JUICED_TEST_MATRIX_CACHE_RAM").unwrap_or_else(|| "0".into()),
        ]);
        match variable("JUICED_TEST_CHAT_TEMPLATE") {
            Some(name) if name == "jinja" => args.push("--jinja".to_owned()),
            Some(name) => args.extend(["--chat-template".to_owned(), name]),
            None => args.extend(["--chat-template".to_owned(), "chatml".to_owned()]),
        }
        let mut server = Server::prepare(true, |config| {
            config["session_cache"] = session_cache;
            config["resident_budget_bytes"] = json!(8 * GIB);
            config["startup_timeout_ms"] = json!(300_000);
            config["request_timeout_ms"] = json!(1_800_000);
            config["models"] = json!([{"id":MODEL,"model_file":model,"command":engine,"provider":"llama-cpp","args":args,"resident_bytes":4 * GIB,"max_inflight":slots,"operations":["chat/completions","completions","responses"],"policy":"on-demand"}]);
        });
        // A model check after the first load would take the slot between two measured requests.
        server.set_environment("JUICED_AUTOMATIC_MODEL_CHECKS", "0");
        server.start().await;
        Self {
            server,
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(1800))
                .build()
                .unwrap(),
            seen: 0,
            filler: 0,
            rows: Vec::new(),
        }
    }

    fn post(&self, path: &str, body: &Value) -> reqwest::RequestBuilder {
        self.client
            .post(format!("{}/v1/{path}", self.server.base))
            .bearer_auth(self.server.read_key().unwrap())
            .json(body)
    }

    async fn settle(&self) {
        for _ in 0..2400 {
            let view = self.server.get("juiced/cache/sessions").await;
            let idle = view["models"][0]["slots"].as_array().is_some_and(|slots| {
                slots
                    .iter()
                    .all(|slot| slot["state"] != "unsaved" && slot["state"] != "busy")
            });
            if idle {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("session snapshots did not settle");
    }

    /// The record of the request just sent, once the service has written it and saved what
    /// it means to save.
    async fn record(&mut self) -> Value {
        let record = self.unsettled_record().await;
        self.settle().await;
        record
    }

    async fn unsettled_record(&mut self) -> Value {
        for _ in 0..1200 {
            let records = self.server.get("juiced/requests").await;
            let newest = records["data"].as_array().and_then(|records| {
                records
                    .iter()
                    .find(|record| record["protocol"] != "internal")
                    .cloned()
            });
            if let Some(record) = newest
                && record["id"].as_u64().is_some_and(|id| id > self.seen)
            {
                self.seen = record["id"].as_u64().unwrap();
                return record;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the request left no record");
    }

    async fn send(&mut self, path: &str, body: Value, header: Option<(&str, &str)>) -> Value {
        let mut request = self.post(path, &body);
        if let Some((name, value)) = header {
            request = request.header(name, value);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let reply: Value = response.json().await.unwrap();
        assert_eq!(status, StatusCode::OK, "{reply}");
        reply
    }

    async fn chat(&mut self, messages: &[Value]) -> (Value, Value) {
        let reply = self
            .send(
                "chat/completions",
                json!({"model":MODEL,"messages":messages,"max_tokens":4,"temperature":0,"stream":false}),
                None,
            )
            .await;
        let record = self.record().await;
        (reply, record)
    }

    /// Sends the conversation and appends the reply, as a chat client does.
    async fn turn(&mut self, messages: &mut Vec<Value>, question: &str) -> Value {
        messages.push(user(question));
        let (reply, record) = self.chat(messages).await;
        messages
            .push(json!({"role":"assistant","content":reply["choices"][0]["message"]["content"]}));
        record
    }

    /// A short unrelated conversation that leaves the slot holding something else.
    async fn take_slot(&mut self) {
        self.filler += 1;
        let text = format!(
            "Unrelated chat number {}. {}",
            self.filler,
            document(10_000 + u64::from(self.filler), 3)
        );
        self.chat(&[user(&text)]).await;
    }

    fn row(&mut self, id: &str, sends: &str, should: &str, record: &Value) -> Value {
        let cache = &record["session_cache"];
        let row = json!({
            "id": id, "sends": sends, "should": should,
            "path": cache["path"], "key": cache["key"], "slot": cache["slot"],
            "input": record["input_tokens"], "cached": record["cached_tokens"],
            "processed": record["prompt_processed_tokens"],
            "elapsed_ms": record["elapsed_ms"], "restore_ms": cache["restore_ms"],
            "saved_previous": cache["saved_previous"], "outcome": record["outcome"],
            "prefill_tokens_per_second": record["prompt_tokens_per_second"].as_f64().map(f64::round),
        });
        println!("cache-matrix {row}");
        self.rows.push(row.clone());
        row
    }

    async fn sessions(&self) -> Value {
        self.server.get("juiced/cache/sessions").await
    }

    fn finish(self) -> Vec<Value> {
        self.rows
    }
}

fn table(rows: &[Value]) {
    println!(
        "\n| id | client sends | should reuse | path | input | cached | processed | ms | restore ms |"
    );
    println!("|---|---|---|---|---|---|---|---|---|");
    for row in rows {
        let cell = |name: &str| match &row[name] {
            Value::Null => "-".to_owned(),
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            cell("id"),
            cell("sends"),
            cell("should"),
            cell("path"),
            cell("input"),
            cell("cached"),
            cell("processed"),
            cell("elapsed_ms"),
            cell("restore_ms"),
        );
    }
}

async fn conversations(lab: &mut Lab, size: usize) {
    let long = format!("You are a careful assistant.\n{}", document(1, size));
    let mut a = vec![system(&long)];
    let record = lab.turn(&mut a, "What does record 1-3 report?").await;
    lab.row(
        "A1",
        "first turn of conversation A (long system prompt)",
        "nothing",
        &record,
    );
    let record = lab.turn(&mut a, "And record 1-4?").await;
    lab.row(
        "A2",
        "next turn, slot still holds A",
        "all earlier turns",
        &record,
    );
    lab.take_slot().await;
    let record = lab.turn(&mut a, "And record 1-5?").await;
    lab.row(
        "A3",
        "next turn after another conversation took the slot",
        "all earlier turns",
        &record,
    );
    let (status, _) = lab.server.post("unload", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK);
    let record = lab.turn(&mut a, "And record 1-6?").await;
    lab.row(
        "A4",
        "next turn after the model unloaded and reloaded",
        "all earlier turns",
        &record,
    );
    let (status, _) = lab.server.post("unload", json!({"model":MODEL})).await;
    assert_eq!(status, StatusCode::OK);
    lab.server.restart().await;
    let record = lab.turn(&mut a, "And record 1-7?").await;
    lab.row(
        "A5",
        "next turn after the service restarted",
        "all earlier turns",
        &record,
    );

    let (_, record) = lab.chat(&[system(&long), user("List two areas.")]).await;
    lab.row(
        "N1",
        "new conversation, same system prompt, slot holds A",
        "the system prompt",
        &record,
    );
    lab.take_slot().await;
    let (_, record) = lab.chat(&[system(&long), user("Name one sensor.")]).await;
    lab.row(
        "N2",
        "new conversation, same system prompt, slot taken",
        "the system prompt",
        &record,
    );

    let last = a.len() - 1;
    let request = a[..last].to_vec();
    lab.take_slot().await;
    let (_, record) = lab.chat(&request).await;
    lab.row(
        "R1",
        "regenerate the last reply, slot taken",
        "everything",
        &record,
    );
    let (_, record) = lab.chat(&request).await;
    lab.row(
        "R2",
        "regenerate again, slot holds it",
        "everything",
        &record,
    );
    let mut edited = a[..last - 1].to_vec();
    edited.push(user("Actually, what about record 1-9?"));
    let (_, record) = lab.chat(&edited).await;
    lab.row(
        "R3",
        "edit the last user message, slot holds the conversation",
        "all earlier turns",
        &record,
    );
    lab.take_slot().await;
    edited.pop();
    edited.push(user("No, record 1-10 instead."));
    let (_, record) = lab.chat(&edited).await;
    lab.row(
        "R4",
        "edit the last user message, slot taken",
        "all earlier turns",
        &record,
    );
    lab.take_slot().await;
    let mut branch = a[..3].to_vec();
    branch.push(user("Different direction: which area is busiest?"));
    let (_, record) = lab.chat(&branch).await;
    lab.row(
        "R5",
        "branch from the first turn, slot taken",
        "system prompt and first turn",
        &record,
    );
}

async fn partial_messages(lab: &mut Lab, size: usize) {
    let dated = |day: u32| {
        format!(
            "House rules follow.\n{}\nToday is day {day} of the year.",
            document(2, size)
        )
    };
    let (_, record) = lab.chat(&[system(&dated(1)), user("Lights?")]).await;
    lab.row(
        "P1",
        "system prompt that ends with a live figure, first time",
        "nothing",
        &record,
    );
    let (_, record) = lab.chat(&[system(&dated(2)), user("Lights?")]).await;
    lab.row(
        "P2",
        "same prompt, figure changed, slot holds the earlier one",
        "the prompt up to the figure",
        &record,
    );
    lab.take_slot().await;
    let (_, record) = lab.chat(&[system(&dated(3)), user("Lights?")]).await;
    lab.row(
        "P3",
        "same prompt, figure changed, slot taken",
        "the prompt up to the figure",
        &record,
    );

    let ask = |question: &str| {
        vec![
            system("Answer from the document."),
            user(&format!("{}\n\nQuestion: {question}", document(3, size))),
        ]
    };
    let (_, record) = lab.chat(&ask("What does record 3-1 report?")).await;
    lab.row(
        "D1",
        "short system prompt, then one long document and a question",
        "nothing",
        &record,
    );
    let (_, record) = lab.chat(&ask("Which area has a lamp?")).await;
    lab.row(
        "D2",
        "same document, other question, slot holds the earlier one",
        "the document",
        &record,
    );
    lab.take_slot().await;
    let (_, record) = lab.chat(&ask("Is any relay listed?")).await;
    lab.row(
        "D3",
        "same document, other question, slot taken",
        "the document",
        &record,
    );

    let early = |day: u32| format!("Today is day {day} of the year.\n{}", document(4, size));
    let (_, record) = lab.chat(&[system(&early(1)), user("Lights?")]).await;
    lab.row(
        "E1",
        "system prompt that opens with a live figure, first time",
        "nothing",
        &record,
    );
    lab.take_slot().await;
    let (_, record) = lab.chat(&[system(&early(2)), user("Lights?")]).await;
    lab.row(
        "E2",
        "same prompt, opening figure changed, slot taken",
        "nothing: the text differs from its start",
        &record,
    );
}

async fn protocols(lab: &mut Lab, size: usize) {
    let instructions = format!("Answer briefly.\n{}", document(5, size));
    let respond = |input: Value| json!({"model":MODEL,"instructions":instructions,"input":input,"max_output_tokens":4,"temperature":0,"stream":false});
    let first = json!({"role":"user","content":"What does record 5-1 report?"});
    let reply = lab.send("responses", respond(json!([first])), None).await;
    let record = lab.record().await;
    lab.row("O1", "responses API, first turn", "nothing", &record);
    lab.take_slot().await;
    let answer = reply["output"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["type"] == "message")
                .map(|item| item["content"][0]["text"].clone())
        })
        .unwrap_or(json!(""));
    let follow = json!([first, {"type":"message","role":"assistant","content":[{"type":"output_text","text":answer}]}, {"role":"user","content":"And 5-2?"}]);
    lab.send("responses", respond(follow), None).await;
    let record = lab.record().await;
    lab.row(
        "O2",
        "responses API, next turn, slot taken",
        "all earlier turns",
        &record,
    );
    lab.take_slot().await;
    lab.send(
        "responses",
        respond(json!([{"role":"user","content":"Name one area."}])),
        None,
    )
    .await;
    let record = lab.record().await;
    lab.row(
        "O3",
        "responses API, new conversation, same instructions, slot taken",
        "the instructions",
        &record,
    );

    let prompt = format!("{}\nSummary:", document(6, size));
    let complete = |prompt: &str| json!({"model":MODEL,"prompt":prompt,"max_tokens":4,"temperature":0,"stream":false});
    let reply = lab.send("completions", complete(&prompt), None).await;
    let record = lab.record().await;
    lab.row("C1", "completions API, long prompt", "nothing", &record);
    lab.take_slot().await;
    let longer = format!(
        "{prompt}{} In more detail:",
        reply["choices"][0]["text"].as_str().unwrap_or("")
    );
    lab.send("completions", complete(&longer), None).await;
    let record = lab.record().await;
    lab.row(
        "C2",
        "completions API, the prompt extended, slot taken",
        "the earlier prompt",
        &record,
    );

    let streamed = format!("Reply in one word.\n{}", document(7, size));
    let stream = async |lab: &mut Lab, messages: &[Value], tokens: u32, stop_early: bool| {
        let response = lab
            .post(
                "chat/completions",
                &json!({"model":MODEL,"messages":messages,"max_tokens":tokens,"temperature":0,"stream":true}),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.bytes_stream();
        let mut text = String::new();
        while let Some(chunk) = body.next().await {
            text.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
            if stop_early && text.matches("\"content\"").count() >= 3 {
                break;
            }
        }
        drop(body);
        text
    };
    let mut s = vec![system(&streamed), user("Area of record 7-1?")];
    stream(lab, &s, 4, false).await;
    let record = lab.record().await;
    lab.row("S1", "streaming chat, first turn", "nothing", &record);
    lab.take_slot().await;
    s.push(json!({"role":"assistant","content":"One."}));
    s.push(user("And 7-2?"));
    stream(lab, &s, 4, false).await;
    let record = lab.record().await;
    lab.row(
        "S2",
        "streaming chat, next turn, slot taken",
        "the first turn's prompt",
        &record,
    );

    let cancelled = format!("Write at length.\n{}", document(8, size));
    let x = vec![system(&cancelled), user("Describe every record in order.")];
    stream(lab, &x, 2000, true).await;
    let record = lab.record().await;
    lab.row(
        "X1",
        "streaming chat, client disconnects during the reply",
        "nothing",
        &record,
    );
    let (_, record) = lab.chat(&x).await;
    lab.row(
        "X2",
        "the same request again, slot untouched since",
        "the whole prompt",
        &record,
    );
    let never_finished = format!("Write at length.\n{}", document(12, size));
    let y = vec![
        system(&never_finished),
        user("Describe every record in order."),
    ];
    stream(lab, &y, 2000, true).await;
    lab.record().await;
    lab.take_slot().await;
    let (_, record) = lab.chat(&y).await;
    lab.row(
        "X3",
        "a request that only ever disconnected, sent again after the slot was taken",
        "nothing: an unfinished request leaves no state",
        &record,
    );
}

async fn headers_and_bypass(lab: &mut Lab, size: usize) {
    let long = format!("You are a careful assistant.\n{}", document(9, size));
    let named = async |lab: &mut Lab, session: &str, messages: &[Value]| {
        lab.send(
            "chat/completions",
            json!({"model":MODEL,"messages":messages,"max_tokens":4,"temperature":0,"stream":false}),
            Some(("x-juiced-session", session)),
        )
        .await;
        lab.record().await
    };
    let mut h = vec![system(&long), user("Record 9-1?")];
    let record = named(lab, "thread-one", &h).await;
    lab.row(
        "H1",
        "x-juiced-session header, first turn",
        "nothing",
        &record,
    );
    lab.take_slot().await;
    h.push(json!({"role":"assistant","content":"Yes."}));
    h.push(user("Record 9-2?"));
    let record = named(lab, "thread-one", &h).await;
    lab.row(
        "H2",
        "same header, next turn, slot taken",
        "the first turn's prompt",
        &record,
    );
    lab.take_slot().await;
    let record = named(lab, "thread-two", &[system(&long), user("Record 9-3?")]).await;
    lab.row(
        "H3",
        "new header value, same system prompt, slot taken",
        "the system prompt",
        &record,
    );

    let mut b = vec![
        system(&format!("Be exact.\n{}", document(11, size))),
        user("Record 11-1?"),
    ];
    let (_, record) = lab.chat(&b).await;
    lab.row("B1", "plain request, first turn", "nothing", &record);
    lab.send(
        "chat/completions",
        json!({"model":MODEL,"messages":b,"max_tokens":4,"temperature":0,"cache_prompt":false}),
        None,
    )
    .await;
    let record = lab.record().await;
    lab.row(
        "B2",
        "the same request with cache_prompt false",
        "nothing: the client asked for a full read",
        &record,
    );
    b.push(json!({"role":"assistant","content":"Yes."}));
    b.push(user("Record 11-2?"));
    let (_, record) = lab.chat(&b).await;
    lab.row(
        "B3",
        "next turn without the flag, slot untouched since",
        "the first turn's prompt",
        &record,
    );
}

#[tokio::test]
#[ignore = "requires JUICED_TEST_LLAMA_SERVER and JUICED_TEST_PREFIX_GGUF (a chat GGUF); prints what the conversation cache reuses per scenario"]
async fn real_cache_matrix_one_slot() {
    let size = lines();
    // Budgets large enough that no row loses a state to eviction.
    let mut lab = Lab::start(
        1,
        json!({"save_delay_ms": 50, "budget_bytes": 16 * GIB, "model_budget_bytes": 16 * GIB}),
    )
    .await;
    conversations(&mut lab, size).await;
    partial_messages(&mut lab, size).await;
    protocols(&mut lab, size).await;
    headers_and_bypass(&mut lab, size).await;
    let view = lab.sessions().await;
    println!(
        "cache-matrix-states {}",
        json!({"sessions": view["totals"]["sessions"], "bytes": view["totals"]["bytes"]})
    );
    table(&lab.finish());
}

#[tokio::test]
#[ignore = "requires JUICED_TEST_LLAMA_SERVER and JUICED_TEST_PREFIX_GGUF (a chat GGUF); two slots with interleaved conversations"]
async fn real_cache_matrix_two_slots() {
    let size = lines();
    let mut lab = Lab::start(2, json!({"save_delay_ms": 50})).await;
    let mut a = vec![system(&format!("Assistant A.\n{}", document(21, size)))];
    let mut b = vec![system(&format!("Assistant B.\n{}", document(22, size)))];
    let mut c = vec![system(&format!("Assistant C.\n{}", document(23, size)))];
    let record = lab.turn(&mut a, "Record 21-1?").await;
    lab.row(
        "T1",
        "two slots: conversation A, first turn",
        "nothing",
        &record,
    );
    let record = lab.turn(&mut b, "Record 22-1?").await;
    lab.row(
        "T2",
        "two slots: conversation B, first turn",
        "nothing",
        &record,
    );
    let record = lab.turn(&mut a, "Record 21-2?").await;
    lab.row("T3", "two slots: A again", "all earlier turns", &record);
    let record = lab.turn(&mut b, "Record 22-2?").await;
    lab.row("T4", "two slots: B again", "all earlier turns", &record);
    let record = lab.turn(&mut c, "Record 23-1?").await;
    lab.row("T5", "two slots: third conversation C", "nothing", &record);
    let record = lab.turn(&mut a, "Record 21-3?").await;
    lab.row(
        "T6",
        "two slots: A after C took a slot",
        "all earlier turns",
        &record,
    );
    let record = lab.turn(&mut b, "Record 22-3?").await;
    lab.row(
        "T7",
        "two slots: B after C took a slot",
        "all earlier turns",
        &record,
    );
    let dated = |day: u32| {
        format!(
            "Assistant D.\n{}\nToday is day {day} of the year.",
            document(24, size)
        )
    };
    let (_, record) = lab.chat(&[system(&dated(1)), user("Lights?")]).await;
    lab.row(
        "T8",
        "two slots: prompt ending in a live figure, first time",
        "nothing",
        &record,
    );
    let (_, record) = lab.chat(&[system(&dated(2)), user("Lights?")]).await;
    lab.row(
        "T9",
        "two slots: figure changed",
        "the prompt up to the figure",
        &record,
    );
    table(&lab.finish());
}

#[tokio::test]
#[ignore = "requires JUICED_TEST_LLAMA_SERVER and JUICED_TEST_PREFIX_GGUF (a chat GGUF); budgets and kept states"]
async fn real_cache_matrix_budgets() {
    let size = lines();
    // One state of this size measures the model's bytes per token before budgets are chosen.
    let mut probe = Lab::start(1, json!({"save_delay_ms": 50})).await;
    let mut a = vec![system(&format!("Assistant A.\n{}", document(31, size)))];
    let record = probe.turn(&mut a, "Record 31-1?").await;
    let view = probe.sessions().await;
    let bytes = view["totals"]["bytes"].as_u64().unwrap();
    let tokens = record["input_tokens"].as_u64().unwrap();
    println!(
        "cache-matrix-state {}",
        json!({"tokens": tokens, "bytes": bytes, "bytes_per_token": bytes / tokens.max(1)})
    );
    drop(probe);

    // Room for two states and the estimate's fixed headroom, but not for a third state.
    let fits_two = bytes * 5 / 2 + 128 * MIB;
    let mut lab = Lab::start(
        1,
        json!({"save_delay_ms": 50, "model_budget_bytes": fits_two, "budget_bytes": 4 * GIB}),
    )
    .await;
    let mut a = vec![system(&format!("Assistant A.\n{}", document(31, size)))];
    let mut b = vec![system(&format!("Assistant B.\n{}", document(32, size)))];
    for round in 1..=3 {
        let record = lab.turn(&mut a, &format!("Record 31-{round}?")).await;
        lab.row(
            &format!("G{}", round * 2 - 1),
            "two long conversations that both fit the budget: A",
            if round == 1 {
                "nothing"
            } else {
                "all earlier turns"
            },
            &record,
        );
        let record = lab.turn(&mut b, &format!("Record 32-{round}?")).await;
        lab.row(
            &format!("G{}", round * 2),
            "two long conversations that both fit the budget: B",
            if round == 1 {
                "nothing"
            } else {
                "all earlier turns"
            },
            &record,
        );
    }
    let view = lab.sessions().await;
    println!(
        "cache-matrix-states {}",
        json!({"budget": fits_two, "sessions": view["totals"]["sessions"], "bytes": view["totals"]["bytes"]})
    );
    let mut rows = lab.finish();

    let fits_one = (bytes * 3 / 2).max(64 * MIB);
    let mut lab = Lab::start(
        1,
        json!({"save_delay_ms": 50, "model_budget_bytes": fits_one, "budget_bytes": 4 * GIB}),
    )
    .await;
    let mut a = vec![system(&format!("Assistant A.\n{}", document(31, size)))];
    let mut b = vec![system(&format!("Assistant B.\n{}", document(32, size)))];
    let record = lab.turn(&mut a, "Record 31-1?").await;
    lab.row("K1", "budget 1.5 states: A, first turn", "nothing", &record);
    let view = lab.sessions().await;
    let (status, kept) = lab
        .server
        .post(
            "juiced/cache/sessions",
            json!({"action":"retain","session":view["sessions"][0]["session"],"retained":true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{kept}");
    let record = lab.turn(&mut b, "Record 32-1?").await;
    lab.row(
        "K2",
        "budget 1.5 states: B while A is kept",
        "nothing",
        &record,
    );
    lab.take_slot().await;
    let record = lab.turn(&mut b, "Record 32-2?").await;
    lab.row(
        "K3",
        "B again, slot taken; A's kept state fills the budget",
        "all earlier turns if B could be saved",
        &record,
    );
    let record = lab.turn(&mut a, "Record 31-2?").await;
    lab.row(
        "K4",
        "kept conversation A returns",
        "all earlier turns",
        &record,
    );
    rows.extend(lab.finish());

    let too_small = (bytes / 2).max(64 * MIB);
    if too_small < bytes {
        let mut lab = Lab::start(
            1,
            json!({"save_delay_ms": 50, "model_budget_bytes": too_small, "budget_bytes": 4 * GIB}),
        )
        .await;
        let mut a = vec![system(&format!("Assistant A.\n{}", document(31, size)))];
        let record = lab.turn(&mut a, "Record 31-1?").await;
        lab.row(
            "L1",
            "state larger than the model budget, first turn",
            "nothing",
            &record,
        );
        lab.take_slot().await;
        let record = lab.turn(&mut a, "Record 31-2?").await;
        lab.row(
            "L2",
            "next turn, slot taken; the state could not be saved",
            "nothing: no state fits the budget",
            &record,
        );
        rows.extend(lab.finish());
    }
    table(&rows);
}

/// One-off requests sent back to back, as a batch job sends them: each arrives before the
/// state of the one before it was saved.
async fn burst(lab: &mut Lab, id: &str, seed: u64, size: usize, no_store: bool) {
    let before = lab.sessions().await["totals"]["bytes"].as_u64().unwrap();
    let started = std::time::Instant::now();
    let mut saved_first = 0;
    for index in 0..4 {
        let text = format!("Classify the records.\n{}", document(seed + index, size));
        lab.send(
            "chat/completions",
            json!({"model":MODEL,"messages":[system(&text), user("Label?")],"max_tokens":4,"temperature":0,"stream":false}),
            no_store.then_some(("x-juiced-cache", "no-store")),
        )
        .await;
        let record = lab.unsettled_record().await;
        saved_first += u64::from(record["session_cache"]["saved_previous"] == true);
    }
    let elapsed = started.elapsed().as_millis() as u64;
    lab.settle().await;
    let after = lab.sessions().await["totals"]["bytes"].as_u64().unwrap();
    let row = json!({
        "id": id,
        "sends": if no_store { "four one-off requests back to back, x-juiced-cache: no-store" } else { "four one-off requests back to back" },
        "should": "nothing; none is continued",
        "path": format!("{saved_first} waited for a save"),
        "elapsed_ms": elapsed,
        "restore_ms": Value::Null,
        "input": Value::Null,
        "cached": Value::Null,
        "processed": format!("{} MiB written", after.saturating_sub(before) / MIB),
    });
    println!("cache-matrix {row}");
    lab.rows.push(row);
}

#[tokio::test]
#[ignore = "requires JUICED_TEST_LLAMA_SERVER and JUICED_TEST_PREFIX_GGUF (a chat GGUF); what one-off requests cost with and without x-juiced-cache: no-store"]
async fn real_cache_matrix_one_off_requests() {
    let size = lines();
    let mut lab = Lab::start(
        1,
        json!({"save_delay_ms": 1000, "budget_bytes": 16 * GIB, "model_budget_bytes": 16 * GIB}),
    )
    .await;
    burst(&mut lab, "U1", 41, size, false).await;
    burst(&mut lab, "U2", 51, size, true).await;
    table(&lab.finish());
}

#[tokio::test]
#[ignore = "requires JUICED_TEST_LLAMA_SERVER and JUICED_TEST_PREFIX_GGUF (a chat GGUF); restore time against prefill time by state size"]
async fn real_cache_restore_cost() {
    let mut lab = Lab::start(
        1,
        json!({"save_delay_ms": 50, "budget_bytes": 16 * GIB, "model_budget_bytes": 16 * GIB}),
    )
    .await;
    println!(
        "\n| prompt tokens | state MiB | cold read ms | prefill tokens/s | restore ms | restore MiB/s | restore equals re-reading (tokens) |"
    );
    println!("|---|---|---|---|---|---|---|");
    for (index, size) in [4, 20, 60, 180, 360].into_iter().enumerate() {
        let before = lab.sessions().await["totals"]["bytes"].as_u64().unwrap();
        let mut a = vec![system(&format!(
            "Assistant.\n{}",
            document(61 + index as u64, size)
        ))];
        let cold = lab.turn(&mut a, "First record?").await;
        let state = lab.sessions().await["totals"]["bytes"].as_u64().unwrap() - before;
        let mut restores = Vec::new();
        for question in ["Second?", "Third?", "Fourth?"] {
            lab.take_slot().await;
            let record = lab.turn(&mut a, question).await;
            assert_eq!(record["session_cache"]["path"], "restored", "{record}");
            restores.push(record["session_cache"]["restore_ms"].as_u64().unwrap());
        }
        restores.sort_unstable();
        let restore = restores[1];
        let rate = cold["prompt_tokens_per_second"].as_f64().unwrap();
        println!(
            "| {} | {} | {} | {:.0} | {} | {:.0} | {:.0} |",
            cold["input_tokens"],
            state / MIB,
            cold["elapsed_ms"],
            rate,
            restore,
            state as f64 / MIB as f64 / (restore.max(1) as f64 / 1000.0),
            restore as f64 * rate / 1000.0,
        );
    }
}
