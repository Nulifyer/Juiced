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
