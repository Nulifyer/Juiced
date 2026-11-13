//! Metadata-only GGUF files whose shape mirrors real llama.cpp layouts.

pub enum Entry {
    Text(&'static str),
    U32(u32),
    Bool(bool),
}

fn string(bytes: &mut Vec<u8>, text: &str) {
    bytes.extend((text.len() as u64).to_le_bytes());
    bytes.extend(text.as_bytes());
}

pub fn build(entries: &[(&str, Entry)]) -> Vec<u8> {
    let mut bytes = b"GGUF".to_vec();
    bytes.extend(3u32.to_le_bytes());
    bytes.extend(0u64.to_le_bytes());
    bytes.extend((entries.len() as u64).to_le_bytes());
    for (key, value) in entries {
        string(&mut bytes, key);
        match value {
            Entry::Text(text) => {
                bytes.extend(8u32.to_le_bytes());
                string(&mut bytes, text);
            }
            Entry::U32(number) => {
                bytes.extend(4u32.to_le_bytes());
                bytes.extend(number.to_le_bytes());
            }
            Entry::Bool(flag) => {
                bytes.extend(7u32.to_le_bytes());
                bytes.push(u8::from(*flag));
            }
        }
    }
    bytes
}

/// `ornith-ai/Ornith-1.5-35B-A3B-GGUF` metadata: qwen35moe with 40 hybrid layers (one in four
/// with attention) and one MTP block, trained for 262144 tokens.
pub fn qwen35moe() -> Vec<u8> {
    use Entry::*;
    build(&[
        ("general.architecture", Text("qwen35moe")),
        ("qwen35moe.context_length", U32(262_144)),
        ("qwen35moe.block_count", U32(41)),
        ("qwen35moe.embedding_length", U32(2048)),
        ("qwen35moe.attention.head_count", U32(16)),
        ("qwen35moe.attention.head_count_kv", U32(2)),
        ("qwen35moe.attention.key_length", U32(256)),
        ("qwen35moe.attention.value_length", U32(256)),
        ("qwen35moe.full_attention_interval", U32(4)),
        ("qwen35moe.nextn_predict_layers", U32(1)),
        ("qwen35moe.ssm.conv_kernel", U32(4)),
        ("qwen35moe.ssm.inner_size", U32(4096)),
        ("qwen35moe.ssm.state_size", U32(128)),
        ("qwen35moe.ssm.group_count", U32(16)),
        ("qwen35moe.vocab_size", U32(248_320)),
    ])
}

/// A dense llama layout (54 blocks, 8 KV heads of width 128) trained for 131072 tokens.
pub fn dense() -> Vec<u8> {
    use Entry::*;
    build(&[
        ("general.architecture", Text("llama")),
        ("llama.context_length", U32(131_072)),
        ("llama.block_count", U32(54)),
        ("llama.embedding_length", U32(5120)),
        ("llama.attention.head_count", U32(32)),
        ("llama.attention.head_count_kv", U32(8)),
        ("llama.attention.key_length", U32(128)),
        ("llama.attention.value_length", U32(128)),
        ("llama.vocab_size", U32(131_072)),
    ])
}

/// Copies a real GGUF file and adds one text metadata entry, keeping tensor data aligned.
pub fn with_text(source: &[u8], key: &str, value: &str) -> Vec<u8> {
    fn scalar(kind: u32) -> usize {
        match kind {
            0 | 1 | 7 => 1,
            2 | 3 => 2,
            4..=6 => 4,
            10..=12 => 8,
            _ => panic!("unsupported GGUF value type {kind}"),
        }
    }
    let mut at = 24usize;
    let read_u32 = |at: &mut usize| {
        let value = u32::from_le_bytes(source[*at..*at + 4].try_into().unwrap());
        *at += 4;
        value
    };
    let read_u64 = |at: &mut usize| {
        let value = u64::from_le_bytes(source[*at..*at + 8].try_into().unwrap());
        *at += 8;
        value as usize
    };
    assert_eq!(&source[..4], b"GGUF");
    let tensors = u64::from_le_bytes(source[8..16].try_into().unwrap()) as usize;
    let entries = u64::from_le_bytes(source[16..24].try_into().unwrap());
    let mut alignment = 32usize;
    for _ in 0..entries {
        let length = read_u64(&mut at);
        let name = std::str::from_utf8(&source[at..at + length])
            .unwrap()
            .to_owned();
        at += length;
        let kind = read_u32(&mut at);
        match kind {
            8 => at += read_u64(&mut at),
            9 => {
                let element = read_u32(&mut at);
                let count = read_u64(&mut at);
                for _ in 0..count {
                    if element == 8 {
                        at += read_u64(&mut at);
                    } else {
                        at += scalar(element);
                    }
                }
            }
            _ => {
                if name == "general.alignment" {
                    alignment = u32::from_le_bytes(source[at..at + 4].try_into().unwrap()) as usize;
                }
                at += scalar(kind);
            }
        }
    }
    let entries_end = at;
    for _ in 0..tensors {
        at += read_u64(&mut at);
        let dimensions = read_u32(&mut at) as usize;
        at += dimensions * 8 + 4 + 8;
    }
    let infos_end = at;
    let data_start = infos_end.div_ceil(alignment) * alignment;
    let mut bytes = source[..16].to_vec();
    bytes.extend((entries + 1).to_le_bytes());
    bytes.extend(&source[24..entries_end]);
    string(&mut bytes, key);
    bytes.extend(8u32.to_le_bytes());
    string(&mut bytes, value);
    bytes.extend(&source[entries_end..infos_end]);
    bytes.resize(bytes.len().div_ceil(alignment) * alignment, 0);
    bytes.extend(&source[data_start..]);
    bytes
}
