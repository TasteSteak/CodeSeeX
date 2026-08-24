# Image Capabilities

CodeSeeX treats image understanding and image generation as separate optional tools. They do not share an enable switch, provider, model, endpoint, credential, or usage record.

## Image Understanding

The `vision_analyze` tool supports two backends:

- **DeepSeek Vision** uses `deepseek-v4-flash-vision-exp` through the official `https://api.deepseek.com/responses` endpoint.
- **Custom model** uses a user-configured OpenAI-compatible `/responses` or `/chat/completions` endpoint.

DeepSeek Vision is a tool capability, not a general Codex model. It is not added to the main CodeSeeX model catalog. The image detail setting supports `auto`, `low`, and `original`; DeepSeek currently documents `auto` as equivalent to `original`.

Image understanding is enabled by default for new installations. Legacy configurations receive a one-time capability-schema migration that enables `vision_analyze`; after that migration, later user changes are preserved. The provider choice does not change the public `vision_analyze` capability ID.

## Image Generation

The `image_gen` tool is configured and enabled independently and remains disabled by default. It supports a custom OpenAI-compatible `/images/generations` endpoint or a Responses endpoint with the `image_generation` tool.

CodeSeeX does not assume that DeepSeek Vision can generate images. Selecting DeepSeek Vision for image understanding never changes the image generation endpoint or credential.

## Inputs And Limits

Image understanding accepts the current Codex `input_image`, HTTP(S) URLs, `data:image` URLs, `file://` URLs, workspace paths, and permitted local absolute paths.

CodeSeeX keeps conservative local limits even when an upstream provider allows larger requests:

- Up to 4 images per `vision_analyze` call.
- Up to 8 MiB per local image.
- Bounded prompt and response sizes.
- Workspace and full-file-access checks for local paths.
- Existing network safety checks for remote URLs.

Files API upload and remote image persistence are not part of 0.7.0.

## Credentials

Custom image understanding and image generation use separate secrets. The config API returns only whether each secret is configured and never returns its value.

DeepSeek Vision reads only an explicit `DEEPSEEK_API_KEY` source. It does not use the CodeSeeX local capability token, blindly forward Codex inbound authorization, or reuse a custom image-generation credential.

On platforms without an available secure credential store, custom image secrets fail closed instead of being written back to plaintext TOML.

## Usage And Cost

Vision usage is recorded as a separate Usage segment. Provider-reported final usage is authoritative; CodeSeeX does not add an image-token estimate again after final usage arrives.

The default off-peak DeepSeek Vision estimates are:

| Token type | CNY / 1M tokens |
| --- | ---: |
| Cached input | 0.05 |
| Cache-miss input | 1.50 |
| Output | 4.50 |

The existing Beijing-time peak multiplier applies when peak/off-peak billing is enabled. Custom providers without configured rates show tokens without inventing a CNY estimate.

## Privacy And Logs

Image pixels leave the machine when a remote image-understanding or image-generation provider is used. Configure only providers you trust.

Default CodeSeeX logs keep provider, backend, model, transport, image count, image detail, duration, and normalized usage. They do not store the original image, inline base64, API key, full prompt, or full provider response.
