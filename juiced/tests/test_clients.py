#!/usr/bin/env python3
"""Actual SDK acceptance against an enabled model; tool checks need a tool model."""

import argparse
import importlib.metadata
import json
import time
from pathlib import Path

import anthropic
import ollama
import openai


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--url", required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--api-key", default="juiced-local")
    parser.add_argument("--tools", action="store_true")
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    url = args.url.rstrip("/")
    model = args.model
    proof = {
        "model": model,
        "versions": {
            name: importlib.metadata.version(name)
            for name in ("openai", "anthropic", "ollama")
        },
        "checks": {},
    }
    client = openai.OpenAI(
        base_url=url + "/v1", api_key=args.api_key, timeout=180, max_retries=0
    )
    claude = anthropic.Anthropic(
        base_url=url, api_key=args.api_key, timeout=180, max_retries=0
    )
    local = ollama.Client(
        host=url, headers={"Authorization": "Bearer " + args.api_key}, timeout=180
    )
    messages = [{"role": "user", "content": "Say hello in one short sentence."}]
    extra = {"chat_template_kwargs": {"enable_thinking": False}}

    def record(name, fn):
        start = time.monotonic()
        result = fn()
        proof["checks"][name] = {
            "seconds": round(time.monotonic() - start, 4),
            **result,
        }
        Path(args.out).write_text(json.dumps(proof, indent=2) + "\n")
        print("PASS:", name, flush=True)

    def discovery():
        assert model in [item.id for item in client.models.list()]
        assert model in [
            item.model.removesuffix(":latest") for item in local.list().models
        ]
        assert model in [item.id for item in claude.models.list()]
        assert claude.models.retrieve(model).display_name == model
        assert "completion" in local.show(model).capabilities
        return {"openai": True, "ollama": True, "anthropic": True}

    def chat():
        response = client.chat.completions.create(
            model=model,
            messages=messages,
            max_tokens=32,
            temperature=0,
            extra_body=extra,
        )
        assert response.choices[0].message.content
        chunks = list(
            client.chat.completions.create(
                model=model,
                messages=messages,
                max_tokens=32,
                stream=True,
                stream_options={"include_usage": True},
                extra_body=extra,
            )
        )
        assert any(item.choices and item.choices[0].delta.content for item in chunks)
        assert chunks[-1].usage is not None
        return {"json": True, "stream_chunks": len(chunks), "usage": True}

    def ollama_chat():
        response = local.chat(
            model=model,
            messages=messages,
            think=False,
            options={"num_predict": 32, "temperature": 0},
        )
        assert response.message.content and response.done
        chunks = list(
            local.chat(
                model=model,
                messages=messages,
                think=False,
                stream=True,
                options={"num_predict": 32, "temperature": 0},
            )
        )
        assert chunks[-1].done and any(item.message.content for item in chunks)
        return {"json": True, "stream_chunks": len(chunks), "done": True}

    def anthropic_chat():
        response = claude.messages.create(
            model=model, messages=messages, max_tokens=32, thinking={"type": "disabled"}
        )
        assert any(getattr(item, "text", "") for item in response.content)
        with claude.messages.stream(
            model=model, messages=messages, max_tokens=32, thinking={"type": "disabled"}
        ) as stream:
            text = "".join(stream.text_stream)
            final = stream.get_final_message()
        assert text and final.stop_reason
        count = claude.messages.count_tokens(model=model, messages=messages)
        assert count.input_tokens > 0
        return {"json": True, "stream": True, "input_tokens": count.input_tokens}

    def responses():
        response = client.responses.create(
            model=model, input="Say hello.", max_output_tokens=32, extra_body=extra
        )
        assert response.output_text
        events = list(
            client.responses.create(
                model=model,
                input="Say hello.",
                max_output_tokens=32,
                stream=True,
                extra_body=extra,
            )
        )
        assert any(event.type == "response.output_text.delta" for event in events)
        assert events[-1].type == "response.completed", [e.type for e in events]
        return {"json": True, "stream_events": len(events), "completed": True}

    record("sdk_discovery", discovery)
    record("openai_chat", chat)
    record("ollama_chat", ollama_chat)
    record("anthropic_messages_and_count", anthropic_chat)
    record("openai_responses", responses)

    if args.tools:
        schema = {
            "type": "object",
            "properties": {"key": {"type": "string"}},
            "required": ["key"],
            "additionalProperties": False,
        }
        tool = {
            "type": "function",
            "function": {
                "name": "lookup_marker",
                "description": "Look up the requested marker. Call this to answer the user.",
                "parameters": schema,
            },
        }
        question = [
            {
                "role": "user",
                "content": "Use lookup_marker with key sample, then repeat the returned marker exactly.",
            }
        ]
        marker = "JUICED-COMPAT-NOV02"

        def openai_tools():
            response = client.chat.completions.create(
                model=model,
                messages=question,
                tools=[tool],
                tool_choice={"type": "function", "function": {"name": "lookup_marker"}},
                max_tokens=128,
                extra_body=extra,
            )
            message = response.choices[0].message
            call = message.tool_calls[0]
            assert (
                call.function.name == "lookup_marker"
                and json.loads(call.function.arguments)["key"] == "sample"
            )
            followup = [
                *question,
                message.model_dump(exclude_none=True),
                {"role": "tool", "tool_call_id": call.id, "content": marker},
            ]
            chunks = list(
                client.chat.completions.create(
                    model=model,
                    messages=followup,
                    max_tokens=64,
                    stream=True,
                    extra_body=extra,
                )
            )
            assert marker in "".join(
                item.choices[0].delta.content or "" for item in chunks if item.choices
            )
            return {"arguments": True, "tool_result": True, "stream": True}

        def anthropic_tools():
            response = claude.messages.create(
                model=model,
                messages=question,
                tools=[
                    {
                        "name": "lookup_marker",
                        "description": "Look up the requested marker",
                        "input_schema": schema,
                    }
                ],
                tool_choice={"type": "tool", "name": "lookup_marker"},
                max_tokens=128,
                thinking={"type": "disabled"},
            )
            call = next(item for item in response.content if item.type == "tool_use")
            assert call.name == "lookup_marker" and call.input["key"] == "sample"
            followup = [
                *question,
                {
                    "role": "assistant",
                    "content": [
                        item.model_dump(exclude_none=True) for item in response.content
                    ],
                },
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": call.id,
                            "content": marker,
                        }
                    ],
                },
            ]
            with claude.messages.stream(
                model=model,
                messages=followup,
                max_tokens=64,
                thinking={"type": "disabled"},
            ) as stream:
                assert marker in "".join(stream.text_stream)
            return {"arguments": True, "tool_result": True, "stream": True}

        def ollama_tools():
            response = local.chat(
                model=model,
                messages=question,
                tools=[tool],
                think=False,
                options={"num_predict": 128, "temperature": 0},
            )
            call = response.message.tool_calls[0]
            assert (
                call.function.name == "lookup_marker"
                and call.function.arguments["key"] == "sample"
            )
            followup = [
                *question,
                response.message.model_dump(exclude_none=True),
                {"role": "tool", "tool_name": "lookup_marker", "content": marker},
            ]
            chunks = list(
                local.chat(
                    model=model,
                    messages=followup,
                    think=False,
                    stream=True,
                    options={"num_predict": 64, "temperature": 0},
                )
            )
            assert marker in "".join(item.message.content or "" for item in chunks)
            return {"arguments": True, "tool_result": True, "stream": True}

        record("openai_tool_roundtrip", openai_tools)
        record("anthropic_tool_roundtrip", anthropic_tools)
        record("ollama_tool_roundtrip", ollama_tools)
    proof["passed"] = True
    Path(args.out).write_text(json.dumps(proof, indent=2) + "\n")


if __name__ == "__main__":
    main()
