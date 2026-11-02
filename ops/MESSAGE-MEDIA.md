# Feishu/Lark message content

Steve reads interactive card schema 1.0 and 2.0, including the message API's flattened and `json_card` representations and WebSocket `user_dsl` content. The adapter produces readable text for context and a bounded, structured representation for the web. The space's recent messages and its IM conversation use the same renderer.

Cards show their title, text, columns, tables, notes, collapsible sections, and operation descriptions. HTTP(S) links open only when clicked. Callback buttons and form controls are read-only; complete those operations in Feishu/Lark. Unknown components retain their available text and an explicit fallback. Long or deeply nested cards show a truncation notice. Card markup cannot execute HTML, load arbitrary remote images, or invoke provider callbacks.

Standalone images and files, and resources embedded in rich-text posts, use the authenticated message resource endpoint. Keys are resolved from the stored message and its receiving connection; neither the browser nor the agent supplies an arbitrary provider key or URL. Pictures support preview, enlargement and download. Other files support download. Raster formats are PNG, JPEG, GIF and WebP; HTML/SVG and other formats are download-only. Downloads are limited to 25 MiB and 30 seconds. Resource access is rechecked against the message and its current visibility. Expired or recalled content is not restored through this endpoint.

Older cards and posts can be reinterpreted on read when their original event is still retained and matches the stored message. This does not replay inbound processing or trigger replies. If the original event has been removed, Steve cannot reconstruct information it never stored.

## Content given to a reply model

The channel reply composer can fetch resources from the current message, keeping bytes in memory under the original space's visibility. It supplies supported images as actual model image blocks, and extracts text from UTF-8 text, PDF and DOCX attachments using the existing document parser. Extracted text passes through the existing secret redaction and remains external material under the channel's persona and disclosure rules. The configured model must support image inputs; unsupported adapters fail explicitly instead of silently discarding images.

Limits per reply: ten resources, 20 MiB of fetched content, 3,750,000 bytes per image, 7,500,000 image bytes in total, and 32 KiB of extracted text. Oversized, corrupted, unavailable or unsupported resources produce a clear unread-content note. Image-only scanned PDFs have no OCR in this path. Audio/video can be downloaded but are not transcribed. This does not grant attachment access to separate research, item or ACP sessions; those retain an explicit unread-content boundary until a source-scoped handoff is implemented.

## Provider limitations

Feishu's message resource API rejects card-embedded images/media (error 234043), folders and stickers. Steve displays available descriptions and directs the reader back to Feishu/Lark. It does not bypass the provider's restrictions. Merged-forward message expansion and sending new attachment/card messages are outside this change.

Protocol evidence: the official message-content and message-resource APIs and [larksuite/cli at f31194b](https://github.com/larksuite/cli/tree/f31194b461afbd75479d12850b0f9cb9427cb762/shortcuts/im/convert_lib). The CLI is a reference, not a runtime dependency. Tests use official-shaped fixtures and an isolated provider emulator; they do not certify a real tenant's permissions or a particular model's interpretation of an image.
