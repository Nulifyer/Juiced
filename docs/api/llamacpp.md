# Use llama.cpp-specific operations

Use these routes when the selected model and llama.cpp backend implement the
operation. Standard chat and embedding requests use the
[OpenAI-compatible API](openai.md).

## Rerank documents

Download and enable a compatible reranking model. Replace the example ID with
its exact public model ID:

```bash
curl --fail-with-body http://localhost:8080/v1/rerank \
  -H 'Content-Type: application/json' \
  -d '{"model":"your-enabled-reranking-model-id","query":"Which document describes a cat?","documents":["A cat sleeps on the chair.","The train leaves at noon."]}'
```

The response contains document relevance scores in the backend's supported
reranking format. A chat model is not automatically a reranking model.

## Inspect tokenization

`POST /v1/tokenize` exposes supported backend tokenization. Its request and
response depend on the selected model and the native forwarding contract.
Use the model's own tokenizer when estimating context requirements.
Anthropic message token counting has its separate
[compatibility route](anthropic.md).

## Manage computation through Cache

Native slot inspection and save/restore/erase routes are operator controls.
They can interfere with active runtime state if used outside its session owner.
Use the [Cache workflow](../guide/cache.md) for normal snapshot management.
Raw slot operations do not provide a stored-conversation API.

See [native management](juiced.md) for current operational discovery and
the detailed implementation contracts.
